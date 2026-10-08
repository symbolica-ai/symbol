use super::*;

struct StepClock(AtomicU64);

impl Clock for StepClock {
    fn now_millis(&self) -> i64 {
        i64::try_from(self.0.load(Ordering::SeqCst)).unwrap()
    }
}

type EntryRow = (String, i64);
type FileRowTuple = (String, i64, ContentHash, i64, i64);
type AllocatedRowTuple = (
    String,
    i64,
    ContentHash,
    i64,
    i64,
    String,
    String,
    Option<String>,
    String,
    i64,
);
type AliasRowTuple = (
    String,
    i64,
    String,
    Option<i64>,
    Option<ContentHash>,
    Option<i64>,
    i64,
);
type AggregateRow = (String, i64, i64);
type ExpiryRow = (
    String,
    i64,
    i64,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<i64>,
    Option<f64>,
    Option<i64>,
    Option<i64>,
    i64,
);

#[derive(Debug, PartialEq)]
struct SiteRows {
    entries: Vec<EntryRow>,
    files: Vec<FileRowTuple>,
    allocated: Vec<AllocatedRowTuple>,
    aliases: Vec<AliasRowTuple>,
    aggregates: Vec<AggregateRow>,
    expiry: Vec<ExpiryRow>,
}

/// Every per-site row, keyed by path, with the site id and the generated
/// manifest (which embeds the site's own name) left out.
fn site_rows(store: &Store, name: &str) -> SiteRows {
    let mut db = store.inner.readers.get();
    let site_id = sites::table
        .filter(sites::name.eq(name))
        .select(sites::id)
        .first::<i64>(&mut *db)
        .unwrap();
    SiteRows {
        entries: site_entries::table
            .filter(site_entries::site_id.eq(site_id))
            .filter(site_entries::path.ne(MANIFEST_PATH))
            .select((site_entries::path, site_entries::kind))
            .order(site_entries::path)
            .load(&mut *db)
            .unwrap(),
        files: files::table
            .filter(files::site_id.eq(site_id))
            .filter(files::path.ne(MANIFEST_PATH))
            .select((
                files::path,
                files::kind,
                files::hash,
                files::size,
                files::modified,
            ))
            .order(files::path)
            .load(&mut *db)
            .unwrap(),
        allocated: allocated_entries::table
            .filter(allocated_entries::site_id.eq(site_id))
            .select((
                allocated_entries::path,
                allocated_entries::kind,
                allocated_entries::hash,
                allocated_entries::size,
                allocated_entries::naming_mode,
                allocated_entries::prefix,
                allocated_entries::suffix,
                allocated_entries::extension,
                allocated_entries::media_type,
                allocated_entries::modified,
            ))
            .order(allocated_entries::path)
            .load(&mut *db)
            .unwrap(),
        aliases: aliases::table
            .filter(aliases::site_id.eq(site_id))
            .select((
                aliases::path,
                aliases::kind,
                aliases::canonical_target,
                aliases::resolved_kind,
                aliases::resolved_hash,
                aliases::resolved_size,
                aliases::modified,
            ))
            .order(aliases::path)
            .load(&mut *db)
            .unwrap(),
        aggregates: path_aggregates::table
            .filter(path_aggregates::site_id.eq(site_id))
            .select((
                path_aggregates::path,
                path_aggregates::logical_bytes,
                path_aggregates::file_count,
            ))
            .order(path_aggregates::path)
            .load(&mut *db)
            .unwrap(),
        expiry: expiry_policies::table
            .filter(expiry_policies::site_id.eq(site_id))
            .select((
                expiry_policies::path,
                expiry_policies::target_kind,
                expiry_policies::mode,
                expiry_policies::duration_seconds,
                expiry_policies::deadline,
                expiry_policies::min_age_seconds,
                expiry_policies::max_age_seconds,
                expiry_policies::max_size_bytes,
                expiry_policies::power,
                expiry_policies::refreshed,
                expiry_policies::own_deadline,
                expiry_policies::size_bytes,
            ))
            .order(expiry_policies::path)
            .load(&mut *db)
            .unwrap(),
    }
}

fn site_identity(store: &Store, name: &str) -> (TreeHash, i64) {
    let mut db = store.inner.readers.get();
    sites::table
        .filter(sites::name.eq(name))
        .select((sites::tree_hash, sites::content_revision))
        .first::<(TreeHash, i64)>(&mut *db)
        .unwrap()
}

fn manifest_rows(store: &Store, name: &str) -> (i64, i64) {
    let mut db = store.inner.readers.get();
    let site_id = sites::table
        .filter(sites::name.eq(name))
        .select(sites::id)
        .first::<i64>(&mut *db)
        .unwrap();
    let entries = site_entries::table
        .filter(site_entries::site_id.eq(site_id))
        .filter(site_entries::path.eq(MANIFEST_PATH))
        .select(count_star())
        .first::<i64>(&mut *db)
        .unwrap();
    let files = files::table
        .filter(files::site_id.eq(site_id))
        .filter(files::path.eq(MANIFEST_PATH))
        .select(count_star())
        .first::<i64>(&mut *db)
        .unwrap();
    (entries, files)
}

/// A directory listing without the manifest entry, whose `modified` is the
/// time each site was last regenerated; its size is compared separately via
/// the listing's byte totals.
fn listing(store: &Store, name: &str, rel: &str) -> String {
    let mut listing = store.list_dir(name, rel).unwrap();
    listing.entries.retain(|entry| entry.name != MANIFEST_PATH);
    format!("{listing:?}")
}

const T0: i64 = 1_700_000_000_000;

/// A store whose clock only moves when a test says so.
struct Fixture {
    _dir: tempfile::TempDir,
    store: Store,
    clock: Arc<StepClock>,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(StepClock(AtomicU64::new(T0.cast_unsigned())));
        let store = Store::with_clock(
            dir.path().to_path_buf(),
            "http://symbol".to_string(),
            Arc::<StepClock>::clone(&clock),
        )
        .unwrap();
        Self {
            _dir: dir,
            store,
            clock,
        }
    }

    fn advance(&self) {
        self.clock.0.fetch_add(1_000, Ordering::SeqCst);
    }

    /// Nested files (two sharing a blob), two allocated entries (one without
    /// an extension), and aliases to a file, a directory and nothing. Each
    /// group is written a second apart so `modified` differs between them.
    /// Returns the first allocated entry's path.
    fn seed_content(&self, site: &str) -> String {
        let store = &self.store;
        store.put_file(site, "index.html", b"root").unwrap();
        self.advance();
        store.put_file(site, "docs/c.md", b"c").unwrap();
        store.put_file(site, "docs/guide/a.md", b"a").unwrap();
        self.advance();
        store.put_file(site, "docs/guide/deep/b.md", b"b").unwrap();
        store.put_file(site, "docs/guide/same.md", b"a").unwrap();
        self.advance();
        let allocated = self.allocate(site, "assets/img", Some("png"), b"payload");
        self.allocate(site, "assets", None, b"plain");
        self.advance();
        let alias = |path, target| AliasSpec { path, target };
        store
            .put_aliases(
                site,
                &[
                    alias("latest", "docs/guide/a.md"),
                    alias("docs-link", "docs"),
                    alias("ghost", "nowhere/at/all"),
                ],
                FileMutationOptions::default(),
            )
            .unwrap();
        self.advance();
        allocated
    }

    fn allocate(&self, site: &str, folder: &str, extension: Option<&str>, bytes: &[u8]) -> String {
        let media_type = if extension.is_some() {
            "image/png"
        } else {
            "application/octet-stream"
        };
        self.store
            .allocate_bytes(
                site,
                bytes,
                AllocationSpec {
                    folder,
                    naming: AllocatedName {
                        prefix: if extension.is_some() { "pre-" } else { "" },
                        suffix: "",
                        extension,
                    },
                    media_type,
                },
                FileMutationOptions::default(),
            )
            .unwrap()
            .path
    }

    /// Relative, decay and absolute policies on the site, a folder, a file,
    /// an allocated entry and an alias.
    fn seed_expiry(&self, site: &str, allocated: &str) {
        let relative = |duration_seconds| Some(ExpiryPolicy::Relative { duration_seconds });
        let decay = Some(ExpiryPolicy::Decay(DecayPolicy {
            min_age_seconds: 50,
            max_age_seconds: 50_000,
            max_size_bytes: 1,
            power: 1.5,
        }));
        let absolute = Some(ExpiryPolicy::Absolute {
            deadline_unix_seconds: 1_700_050_000,
        });
        for (rel, policy) in [
            ("", relative(100_000)),
            ("docs", decay),
            ("docs/guide/a.md", absolute),
            (allocated, relative(90_000)),
            ("latest", relative(80_000)),
        ] {
            self.store.set_expiry(site, rel, policy).unwrap();
        }
        self.advance();
    }
}

#[test]
fn copy_site_reproduces_every_row_of_the_source() {
    let fixture = Fixture::new();
    let store = &fixture.store;
    let allocated = fixture.seed_content("src");
    fixture.seed_expiry("src", &allocated);

    let source = site_rows(store, "src");
    assert!(source.files.len() >= 5, "{source:?}");
    assert_eq!(source.allocated.len(), 2);
    assert_eq!(source.aliases.len(), 3);
    assert!(source.aliases.iter().any(|alias| alias.3.is_none()));
    assert!(source.expiry.len() >= 5, "{source:?}");
    assert!(source.aggregates.len() >= 5);

    let (copy, result) = store.copy_site("src", Some("dst"), None).unwrap();
    assert_eq!(copy, "dst");
    assert!(result.created);

    let mut destination = site_rows(store, "dst");
    restore_source_expiry_clock(&mut destination.expiry, T0 + 6_000, T0 + 5_000);
    assert_eq!(destination.entries, source.entries);
    assert_eq!(destination.files, source.files, "includes `modified`");
    assert_eq!(destination.allocated, source.allocated);
    assert_eq!(destination.aliases, source.aliases);
    assert_eq!(destination.aggregates, source.aggregates);
    assert_eq!(destination.expiry, source.expiry);
    assert_eq!(site_identity(store, "dst"), site_identity(store, "src"));
    assert_eq!(manifest_rows(store, "src"), (1, 1));
    assert_eq!(manifest_rows(store, "dst"), (1, 1));
    assert_eq!(result.files, source.files.len() + source.allocated.len());
    assert_same_views(store, &allocated);
}

/// A copy restarts every relative and decay policy's clock at the copy time
/// (`refreshed`, and the deadline derived from it); everything else in a
/// policy row is carried over unchanged. Winds the copy's rows back to the
/// source's clock so the two can be compared.
fn restore_source_expiry_clock(rows: &mut [ExpiryRow], copied_at: i64, source_refreshed: i64) {
    for row in rows {
        if let Some(refreshed) = row.9 {
            assert_eq!(refreshed, copied_at, "policy {:?}", row.0);
            row.9 = Some(source_refreshed);
            row.10 = row
                .10
                .map(|deadline| deadline - (copied_at - source_refreshed));
        }
    }
}

fn assert_same_views(store: &Store, allocated: &str) {
    for rel in ["", "docs", "docs/guide", "docs/guide/deep", "assets"] {
        assert_eq!(
            listing(store, "dst", rel),
            listing(store, "src", rel),
            "listing of {rel:?}"
        );
    }
    for path in ["index.html", "docs/guide/a.md", allocated] {
        assert_eq!(
            node_summary(store, "dst", path),
            node_summary(store, "src", path)
        );
    }
}

fn node_summary(store: &Store, site: &str, path: &str) -> String {
    let node = store.lookup(site, path).unwrap();
    if let Node::File { hash, .. } = &node {
        format!("file {hash:?}")
    } else {
        format!("{node:?}")
    }
}

type UndoFileDeltaRow = (
    String,
    i64,
    Option<i64>,
    Option<ContentHash>,
    Option<i64>,
    Option<i64>,
);
type UndoAllocatedDeltaRow = (
    String,
    i64,
    Option<ContentHash>,
    Option<i64>,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<i64>,
);
type UndoAliasDeltaRow = (
    String,
    i64,
    Option<String>,
    Option<i64>,
    Option<ContentHash>,
    Option<i64>,
    Option<i64>,
);

fn debug_lines<T: std::fmt::Debug>(label: &str, rows: &[T]) -> Vec<String> {
    rows.iter().map(|row| format!("{label} {row:?}")).collect()
}

/// The undo record's header rows: operation, filing name and site state.
fn undo_header_rows(db: &mut SqliteConnection, token: &str) -> Vec<String> {
    let operations = undo_operations::table
        .filter(undo_operations::token.eq(token))
        .select((
            undo_operations::kind,
            undo_operations::description,
            undo_operations::created,
            undo_operations::expires,
            undo_operations::consumed,
        ))
        .load::<(i64, String, i64, i64, i64)>(db)
        .unwrap();
    let names = undo_names::table
        .filter(undo_names::token.eq(token))
        .select(undo_names::name)
        .order(undo_names::name)
        .load::<String>(db)
        .unwrap();
    let sites = undo_sites::table
        .filter(undo_sites::token.eq(token))
        .select((
            undo_sites::name,
            undo_sites::existed,
            undo_sites::public_url,
            undo_sites::created,
            undo_sites::updated,
            undo_sites::content_revision,
            undo_sites::tree_hash,
        ))
        .load::<(String, i64, String, Option<i64>, i64, i64, TreeHash)>(db)
        .unwrap();
    let mut lines = debug_lines("operation", &operations);
    lines.extend(debug_lines("name", &names));
    lines.extend(debug_lines("site", &sites));
    lines
}

/// The undo record's file rows: whole-site copies and per-path deltas.
fn undo_file_rows(db: &mut SqliteConnection, token: &str) -> Vec<String> {
    let saved = undo_files::table
        .filter(undo_files::token.eq(token))
        .select((
            undo_files::path,
            undo_files::hash,
            undo_files::size,
            undo_files::modified,
        ))
        .order(undo_files::path)
        .load::<(String, ContentHash, i64, Option<i64>)>(db)
        .unwrap();
    let deltas = undo_file_deltas::table
        .filter(undo_file_deltas::token.eq(token))
        .select((
            undo_file_deltas::path,
            undo_file_deltas::existed,
            undo_file_deltas::kind,
            undo_file_deltas::hash,
            undo_file_deltas::size,
            undo_file_deltas::modified,
        ))
        .order(undo_file_deltas::path)
        .load::<UndoFileDeltaRow>(db)
        .unwrap();
    let mut lines = debug_lines("file", &saved);
    lines.extend(debug_lines("file-delta", &deltas));
    lines
}

/// The undo record's allocated-entry and alias deltas.
fn undo_typed_rows(db: &mut SqliteConnection, token: &str) -> Vec<String> {
    let allocated = undo_allocated_deltas::table
        .filter(undo_allocated_deltas::token.eq(token))
        .select((
            undo_allocated_deltas::path,
            undo_allocated_deltas::existed,
            undo_allocated_deltas::hash,
            undo_allocated_deltas::size,
            undo_allocated_deltas::naming_mode,
            undo_allocated_deltas::prefix,
            undo_allocated_deltas::suffix,
            undo_allocated_deltas::extension,
            undo_allocated_deltas::media_type,
            undo_allocated_deltas::modified,
        ))
        .order(undo_allocated_deltas::path)
        .load::<UndoAllocatedDeltaRow>(db)
        .unwrap();
    let aliases = undo_alias_deltas::table
        .filter(undo_alias_deltas::token.eq(token))
        .select((
            undo_alias_deltas::path,
            undo_alias_deltas::existed,
            undo_alias_deltas::canonical_target,
            undo_alias_deltas::resolved_kind,
            undo_alias_deltas::resolved_hash,
            undo_alias_deltas::resolved_size,
            undo_alias_deltas::modified,
        ))
        .order(undo_alias_deltas::path)
        .load::<UndoAliasDeltaRow>(db)
        .unwrap();
    let mut lines = debug_lines("allocated-delta", &allocated);
    lines.extend(debug_lines("alias-delta", &aliases));
    lines
}

/// Every undo row recorded for the site's newest undo entry, one line per
/// row. The token itself is random and left out.
fn latest_undo(store: &Store, site: &str) -> Vec<String> {
    let stack = store.undo_stack(site).unwrap();
    let token = &stack.entries[0].token;
    let mut db = store.inner.readers.get();
    let mut lines = undo_header_rows(&mut db, token);
    lines.extend(undo_file_rows(&mut db, token));
    lines.extend(undo_typed_rows(&mut db, token));
    lines
}

fn put_alias(store: &Store, path: &str, target: &str) {
    store
        .put_alias("u", path, target, FileMutationOptions::default())
        .unwrap();
}

/// Runs the operations that open every kind of undo record, in order, and
/// renders each record. The golden file was produced by the per-column
/// `values((..))` inserts these records were first written with.
fn undo_scenario(fixture: &Fixture) -> String {
    let store = &fixture.store;
    let mut dump = Vec::new();
    store.put_file("u", "a.txt", b"one").unwrap();
    fixture.advance();
    store.put_file("u", "a.txt", b"two").unwrap();
    dump.push(("overwrite file", latest_undo(store, "u")));
    fixture.advance();
    store.put_file("u", "b.txt", b"x").unwrap();
    dump.push(("new file", latest_undo(store, "u")));
    fixture.advance();
    put_alias(store, "ln", "a.txt");
    dump.push(("new alias", latest_undo(store, "u")));
    fixture.advance();
    put_alias(store, "ln", "b.txt");
    dump.push(("retarget alias", latest_undo(store, "u")));
    fixture.advance();
    put_alias(store, "dangling", "nowhere");
    put_alias(store, "dangling", "a.txt");
    dump.push(("retarget dangling alias", latest_undo(store, "u")));
    fixture.advance();
    let first = fixture.allocate("u", "assets", Some("png"), b"payload");
    dump.push(("allocate", latest_undo(store, "u")));
    fixture.advance();
    fixture.allocate("u", "", None, b"plain");
    store.delete_file("u", &first).unwrap();
    dump.push(("delete allocated", latest_undo(store, "u")));
    fixture.advance();
    store.delete_file("u", "a.txt").unwrap();
    dump.push(("delete file", latest_undo(store, "u")));
    fixture.advance();
    store.copy_site("u", Some("v"), None).unwrap();
    dump.push(("copy (destination did not exist)", latest_undo(store, "v")));
    fixture.advance();
    store.pop_site("u").unwrap();
    dump.push(("delete site", latest_undo(store, "u")));
    dump.iter().fold(String::new(), |mut out, (label, lines)| {
        writeln!(out, "== {label}\n{}", lines.join("\n")).unwrap();
        out
    })
}

#[test]
fn undo_snapshots_record_nulls_for_missing_entries() {
    let rendered = undo_scenario(&Fixture::new());
    assert_eq!(rendered, include_str!("copy_tests.undo_rows.txt"));
}

#[test]
fn entry_delta_undo_restores_the_exact_rows() {
    let fixture = Fixture::new();
    let store = &fixture.store;
    store.put_file("u", "dir/a.txt", b"one").unwrap();
    fixture.advance();
    let with_extension = fixture.allocate("u", "assets", Some("png"), b"png");
    fixture.advance();
    let bare = fixture.allocate("u", "", None, b"bare");
    fixture.advance();
    put_alias(store, "ghost", "nowhere");
    put_alias(store, "ln", "dir/a.txt");
    fixture.advance();

    let before = site_rows(store, "u");
    let check = |label: &str| {
        fixture.advance();
        store.undo("u", None).unwrap();
        assert_eq!(site_rows(store, "u"), before, "after undoing {label}");
    };
    store.put_file("u", "dir/a.txt", b"two").unwrap();
    check("overwrite");
    store.delete_file("u", "dir/a.txt").unwrap();
    check("delete file");
    store.delete_file("u", &with_extension).unwrap();
    check("delete allocated entry with an extension");
    store.delete_file("u", &bare).unwrap();
    check("delete allocated entry without an extension");
    put_alias(store, "ghost", "dir/a.txt");
    check("retarget dangling alias");
    put_alias(store, "ln", "nowhere");
    check("dangle a resolved alias");
    store.delete_file("u", "ghost").unwrap();
    check("delete alias");
}

#[test]
fn deleting_a_site_snapshots_every_file_row_and_undo_restores_them() {
    let fixture = Fixture::new();
    let store = &fixture.store;
    for (path, bytes) in [("a.txt", "one"), ("dir/b.txt", "two"), ("c.txt", "three")] {
        store.put_file("u", path, bytes.as_bytes()).unwrap();
        fixture.advance();
    }
    fixture.allocate("u", "assets", Some("png"), b"payload");
    fixture.advance();
    put_alias(store, "ln", "a.txt");
    put_alias(store, "ghost", "nowhere");
    fixture.advance();

    let before = site_rows(store, "u");
    store.pop_site("u").unwrap();
    let token = store.undo_stack("u").unwrap().entries[0].token.clone();
    let saved = {
        let mut db = store.inner.readers.get();
        undo_files::table
            .filter(undo_files::token.eq(&token))
            .select((
                undo_files::path,
                undo_files::hash,
                undo_files::size,
                undo_files::modified,
            ))
            .order(undo_files::path)
            .load::<(String, ContentHash, i64, Option<i64>)>(&mut *db)
            .unwrap()
    };
    let expected = before
        .files
        .iter()
        .map(|(path, _, hash, size, modified)| (path.clone(), *hash, *size, Some(*modified)))
        .collect::<Vec<_>>();
    assert_eq!(expected.len(), 3);
    assert_eq!(saved, expected);

    fixture.advance();
    store.undo("u", None).unwrap();
    assert_eq!(site_rows(store, "u"), before);
}
