//! The SQL behind search: the candidate query of name search, and the
//! derived content index with its queries.
//!
//! For name search, SQL only narrows: it returns every file whose path
//! contains each literal fragment of the query. Matching names, deriving
//! folders, ranking and paging happen in `crate::search`. Every file below a
//! matching folder contains that folder's name in its path, so the
//! candidates are enough to count and size each matching folder exactly.
//!
//! Content search reads `search.sqlite`, a derived index beside the main
//! database that `crate::search::index` fills. It is keyed by blob hash, so a
//! document is extracted once however many paths reference it, and every
//! query joins the live `files` table, so deletes, moves and replacements are
//! reflected at once; only brand-new content lags until it is indexed. Each
//! reader connection has the file attached read-only as `search`; the indexer
//! has its own writable connection with the main database attached read-only
//! as `live`. The main schema, its hash and the publish transactions are
//! untouched, and a backup can skip the file.

use diesel::sql_query;
use diesel::sql_types::{Binary, Integer, Nullable};

#[allow(clippy::wildcard_imports)] // see the note in the parent module
use super::*;
use crate::search::extract::Document;

/// The derived content index, beside `symbol.db`.
pub const SEARCH_FILE: &str = "search.sqlite";

/// The extractor and DDL version the index was built with, kept in
/// `PRAGMA user_version`. Bump it when either changes: a mismatch at startup
/// drops the tables and the indexer rebuilds everything.
pub const SEARCH_FORMAT: i32 = 1;

const SEARCH_DDL: &str = "
    CREATE TABLE IF NOT EXISTS docs (
        id INTEGER PRIMARY KEY,
        hash BLOB NOT NULL UNIQUE,
        title TEXT NOT NULL,
        state INTEGER NOT NULL
    );
    CREATE VIRTUAL TABLE IF NOT EXISTS fts USING fts5(
        title, body,
        tokenize = 'porter unicode61 remove_diacritics 2',
        prefix = '2 3'
    );";

/// `docs.state`: the blob's text is in `fts`.
const DOC_INDEXED: i64 = 0;
/// `docs.state`: the blob is not text, or is empty or too large, and is
/// never read again.
const DOC_SKIPPED: i64 = 1;

/// Title hits weigh this much more than body hits in `bm25`.
const TITLE_WEIGHT: &str = "10.0";
/// Tokens of context around the hits in a snippet.
const SNIPPET_TOKENS: &str = "16";

/// A live blob the index has not looked at yet.
#[derive(Debug, Clone, PartialEq, Eq, QueryableByName)]
pub struct Unindexed {
    #[diesel(sql_type = Binary)]
    pub hash: ContentHash,
    /// One of its paths, the first in order; the extractor reads the type
    /// from it.
    #[diesel(sql_type = Text)]
    pub path: String,
    #[diesel(sql_type = BigInt)]
    pub size: i64,
}

/// One page of content hits, each a live file whose blob matched.
#[derive(Debug, Clone, PartialEq, Eq, QueryableByName)]
pub struct ContentHit {
    #[diesel(sql_type = Text)]
    pub site: String,
    #[diesel(sql_type = Text)]
    pub path: String,
    #[diesel(sql_type = BigInt)]
    pub bytes: i64,
    #[diesel(sql_type = BigInt)]
    pub modified: i64,
    #[diesel(sql_type = Text)]
    pub title: String,
    /// Body text around the hits, each wrapped in the markers
    /// `crate::search::extract::HIT_START` and `HIT_END`.
    #[diesel(sql_type = Nullable<Text>)]
    pub snippet: Option<String>,
}

#[derive(QueryableByName)]
struct UserVersion {
    #[diesel(sql_type = Integer)]
    user_version: i32,
}

#[derive(QueryableByName)]
struct Count {
    #[diesel(sql_type = BigInt)]
    total: i64,
}

#[derive(QueryableByName)]
struct Id {
    #[diesel(sql_type = BigInt)]
    id: i64,
}

/// `'text'` for SQL that cannot take a bound parameter, such as `ATTACH`.
fn sql_literal(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// `ATTACH` for a database file, read-only.
pub(super) fn attach_read_only(path: &Path, alias: &str) -> String {
    format!(
        "ATTACH DATABASE {} AS {alias};",
        sql_literal(&format!("file:{}?mode=ro", path.display()))
    )
}

/// Opens the indexer's connection to `search.sqlite` under `root`, creating
/// the index, or recreating it when it was built by another format, and
/// attaches the main database at `main` read-only as `live`. Called before
/// the readers open, so the file and its tables exist when they attach it.
pub(super) fn open_search_index(root: &Path, main: &Path) -> Result<SqliteConnection, StoreError> {
    let path = root.join(SEARCH_FILE);
    let mut db = SqliteConnection::establish(&format!("file:{}", path.display()))?;
    db.batch_execute(
        "PRAGMA busy_timeout = 5000;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;",
    )?;
    let version = sql_query("PRAGMA user_version")
        .get_result::<UserVersion>(&mut db)?
        .user_version;
    let rebuild = version != SEARCH_FORMAT;
    if rebuild {
        if version != 0 {
            tracing::info!(
                found = version,
                wanted = SEARCH_FORMAT,
                "rebuilding the content index: its format changed"
            );
        }
        db.batch_execute("DROP TABLE IF EXISTS fts; DROP TABLE IF EXISTS docs;")?;
    }
    db.batch_execute(SEARCH_DDL)?;
    db.batch_execute(&format!("PRAGMA user_version = {SEARCH_FORMAT};"))?;
    if rebuild && version != 0 {
        db.batch_execute("VACUUM;")?;
    }
    db.batch_execute(&attach_read_only(main, "live"))?;
    Ok(db)
}

/// The scan behind [`Store::search_unindexed`] and [`Store::search_pending`]:
/// one row per live blob that `docs` does not hold. The generated manifest is
/// never indexed.
const UNINDEXED: &str = "FROM live.files AS f
    WHERE f.path <> ?1 AND NOT EXISTS (SELECT 1 FROM docs AS d WHERE d.hash = f.hash)
    GROUP BY f.hash";

/// The joins behind content search: FTS rows to their blobs, to every live
/// file holding that blob. `?1` is the match expression, `?2` the site,
/// `?3`/`?4` the path range and `?7` the manifest path; `+f.path` keeps the
/// range from becoming the index choice, which the hash join should be.
fn content_from(site: bool, range: bool) -> String {
    let mut sql = String::from(
        "FROM search.fts AS fts
         JOIN search.docs AS d ON d.id = fts.rowid
         JOIN files AS f ON f.hash = d.hash",
    );
    if site {
        sql.push_str(" AND f.site_id = ?2");
    }
    sql.push_str(
        " JOIN sites AS s ON s.id = f.site_id
         WHERE fts MATCH ?1 AND f.path <> ?7",
    );
    if range {
        sql.push_str(" AND +f.path >= ?3 AND +f.path < ?4");
    }
    sql
}

impl Store {
    /// Up to `limit` live blobs the index has not looked at, in hash order.
    pub fn search_unindexed(&self, limit: usize) -> Result<Vec<Unindexed>, StoreError> {
        let mut db = self.inner.search.lock().unwrap();
        Ok(sql_query(format!(
            "SELECT f.hash AS hash, min(f.path) AS path, f.size AS size {UNINDEXED}
             ORDER BY f.hash LIMIT ?2"
        ))
        .bind::<Text, _>(MANIFEST_PATH)
        .bind::<BigInt, _>(i64::try_from(limit).unwrap_or(i64::MAX))
        .load(&mut *db)?)
    }

    /// How many live blobs the index has not looked at.
    pub fn search_pending(&self) -> Result<u64, StoreError> {
        let mut db = self.inner.search.lock().unwrap();
        let count = sql_query(format!(
            "SELECT count(*) AS total FROM (SELECT 1 {UNINDEXED})"
        ))
        .bind::<Text, _>(MANIFEST_PATH)
        .get_result::<Count>(&mut *db)?;
        Ok(count.total.cast_unsigned())
    }

    /// Records what the extractor made of each blob, in one transaction: a
    /// `docs` row either way, and the text in `fts` when there is some. A
    /// blob already recorded is left alone.
    pub fn index_search_documents(
        &self,
        documents: &[(ContentHash, Option<Document>)],
    ) -> Result<(), StoreError> {
        let mut db = self.inner.search.lock().unwrap();
        let mut transaction = DbTransaction::begin(&mut db)?;
        for (hash, document) in documents {
            let (title, state) = document.as_ref().map_or(("", DOC_SKIPPED), |document| {
                (document.title.as_str(), DOC_INDEXED)
            });
            let inserted = sql_query(
                "INSERT INTO docs (hash, title, state) VALUES (?1, ?2, ?3)
                 ON CONFLICT (hash) DO NOTHING RETURNING id",
            )
            .bind::<Binary, _>(hash)
            .bind::<Text, _>(title)
            .bind::<BigInt, _>(state)
            .load::<Id>(&mut *transaction)?;
            if let (Some(row), Some(document)) = (inserted.first(), document) {
                sql_query("INSERT INTO fts (rowid, title, body) VALUES (?1, ?2, ?3)")
                    .bind::<BigInt, _>(row.id)
                    .bind::<Text, _>(&document.title)
                    .bind::<Text, _>(&document.body)
                    .execute(&mut *transaction)?;
            }
        }
        transaction.commit()?;
        drop(db);
        Ok(())
    }

    /// Merges the FTS b-trees after a backfill, so queries read fewer pages.
    pub fn search_optimize(&self) -> Result<(), StoreError> {
        let mut db = self.inner.search.lock().unwrap();
        db.batch_execute("INSERT INTO fts (fts) VALUES ('optimize');")?;
        drop(db);
        Ok(())
    }

    /// Drops the documents of blobs no live file references any more, and
    /// says how many. Queries already ignore them; this reclaims space.
    pub fn search_collect_garbage(&self) -> Result<usize, StoreError> {
        let mut db = self.inner.search.lock().unwrap();
        let mut transaction = DbTransaction::begin(&mut db)?;
        sql_query(
            "DELETE FROM fts WHERE rowid IN
             (SELECT id FROM docs WHERE hash NOT IN (SELECT hash FROM live.files))",
        )
        .execute(&mut *transaction)?;
        let removed = sql_query("DELETE FROM docs WHERE hash NOT IN (SELECT hash FROM live.files)")
            .execute(&mut *transaction)?;
        transaction.commit()?;
        drop(db);
        Ok(removed)
    }

    /// How many blobs the index holds text for.
    pub fn search_documents(&self) -> Result<u64, StoreError> {
        let mut db = self.inner.readers.get();
        let count = sql_query("SELECT count(*) AS total FROM search.docs WHERE state = ?1")
            .bind::<BigInt, _>(DOC_INDEXED)
            .get_result::<Count>(&mut *db)?;
        Ok(count.total.cast_unsigned())
    }

    /// The live files whose text matches `expression`, an FTS5 expression
    /// from `crate::search::query`, in `site` (or every site) and below
    /// `prefix`: how many in all, and the page at `offset` of `limit` hits,
    /// best first. Ranking is `bm25` with the title weighed ten times the
    /// body, then site and path, so pages are stable. The snippet is computed
    /// for the page only, from the body, so every hit shows context even
    /// when the title matched.
    pub fn search_content(
        &self,
        site: Option<&str>,
        prefix: &str,
        expression: &str,
        limit: usize,
        offset: usize,
    ) -> Result<(u64, Vec<ContentHit>), StoreError> {
        let site = site.map(parse_site_name).transpose()?;
        let prefix_end = prefix
            .strip_suffix('/')
            .map(|directory| format!("{directory}0"));
        let mut db = self.inner.readers.get();
        let mut snapshot = DbTransaction::begin(&mut db)?;
        let site_id = site
            .map(|name| site_id_locked(&mut snapshot, name))
            .transpose()?;
        let from = content_from(site_id.is_some(), prefix_end.is_some());
        let bind = |query: diesel::query_builder::SqlQuery| {
            query
                .bind::<Text, _>(expression)
                .bind::<BigInt, _>(site_id.unwrap_or_default())
                .bind::<Text, _>(prefix)
                .bind::<Text, _>(prefix_end.clone().unwrap_or_default())
                .bind::<BigInt, _>(i64::try_from(limit).unwrap_or(i64::MAX))
                .bind::<BigInt, _>(i64::try_from(offset).unwrap_or(i64::MAX))
                .bind::<Text, _>(MANIFEST_PATH)
        };
        let total = bind(sql_query(format!("SELECT count(*) AS total {from}")))
            .get_result::<Count>(&mut *snapshot)?
            .total
            .cast_unsigned();
        let hits = bind(sql_query(format!(
            "WITH top AS MATERIALIZED (
                SELECT fts.rowid AS id, s.name AS site, f.path AS path, f.size AS bytes,
                       f.modified AS modified, d.title AS title,
                       bm25(fts, {TITLE_WEIGHT}, 1.0) AS rank
                {from}
                ORDER BY rank, s.name, f.path LIMIT ?5 OFFSET ?6)
             SELECT top.site, top.path, top.bytes, top.modified, top.title,
                    (SELECT snippet(fts, 1, char(57344), char(57345), '…', {SNIPPET_TOKENS})
                     FROM search.fts AS fts WHERE fts MATCH ?1 AND fts.rowid = top.id) AS snippet
             FROM top ORDER BY top.rank, top.site, top.path"
        )))
        .load::<ContentHit>(&mut *snapshot)?;
        snapshot.commit()?;
        Ok((total, hits))
    }
}

/// A file that may match a name search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchCandidate {
    pub site: String,
    pub path: String,
    pub bytes: u64,
    pub modified: i64,
}

/// `%fragment%` for `LIKE … ESCAPE '\'`.
fn contains_pattern(fragment: &str) -> String {
    let mut pattern = String::with_capacity(fragment.len() + 2);
    pattern.push('%');
    for character in fragment.chars() {
        if matches!(character, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push('%');
    pattern
}

/// A search scope directory as `dir/`, or empty for the whole site.
pub fn normalize_search_prefix(raw: &str) -> Result<String, StoreError> {
    let directory = normalize_rel(raw.trim_matches('/'))?;
    Ok(if directory.is_empty() {
        directory
    } else {
        format!("{directory}/")
    })
}

impl Store {
    /// Every site name, sorted.
    pub fn site_names(&self) -> Result<Vec<String>, StoreError> {
        let mut db = self.inner.readers.get();
        Ok(sites::table
            .select(sites::name)
            .order(sites::name)
            .load(&mut *db)?)
    }

    /// Files and allocated entries whose path contains every fragment,
    /// case-insensitively for ASCII, in `site` (or every site) and below
    /// `prefix` (a normalized directory ending in `/`, or empty).
    ///
    /// The generated `symbol.toml` is never a candidate.
    pub fn search_candidates(
        &self,
        site: Option<&str>,
        prefix: &str,
        fragments: &[String],
    ) -> Result<Vec<SearchCandidate>, StoreError> {
        let site = site.map(parse_site_name).transpose()?;
        // Paths below `dir/` sort between `dir/` and `dir0`, `0` being the
        // character after `/`.
        let prefix_end = prefix
            .strip_suffix('/')
            .map(|directory| format!("{directory}0"));
        let patterns: Vec<String> = fragments.iter().map(|f| contains_pattern(f)).collect();
        let mut db = self.inner.readers.get();
        let mut snapshot = DbTransaction::begin(&mut db)?;
        let site_id = site
            .map(|name| site_id_locked(&mut snapshot, name))
            .transpose()?;

        let mut query = files::table
            .inner_join(sites::table)
            .filter(files::path.ne(MANIFEST_PATH))
            .select((sites::name, files::path, files::size, files::modified))
            .into_boxed();
        if let Some(site_id) = site_id {
            query = query.filter(files::site_id.eq(site_id));
        }
        if let Some(end) = &prefix_end {
            query = query.filter(files::path.ge(prefix).and(files::path.lt(end)));
        }
        for pattern in &patterns {
            query = query.filter(files::path.like(pattern).escape('\\'));
        }
        let mut rows = query.load::<(String, String, i64, i64)>(&mut *snapshot)?;

        let mut allocated = allocated_entries::table
            .inner_join(sites::table)
            .select((
                sites::name,
                allocated_entries::path,
                allocated_entries::size,
                allocated_entries::modified,
            ))
            .into_boxed();
        if let Some(site_id) = site_id {
            allocated = allocated.filter(allocated_entries::site_id.eq(site_id));
        }
        if let Some(end) = &prefix_end {
            allocated = allocated.filter(
                allocated_entries::path
                    .ge(prefix)
                    .and(allocated_entries::path.lt(end)),
            );
        }
        for pattern in &patterns {
            allocated = allocated.filter(allocated_entries::path.like(pattern).escape('\\'));
        }
        rows.extend(allocated.load::<(String, String, i64, i64)>(&mut *snapshot)?);
        snapshot.commit()?;

        Ok(rows
            .into_iter()
            .map(|(site, path, size, modified)| SearchCandidate {
                site,
                path,
                bytes: size.cast_unsigned(),
                modified,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::extract;

    #[test]
    fn like_wildcards_in_fragments_are_literal() {
        assert_eq!(contains_pattern("kan-ext"), "%kan-ext%");
        assert_eq!(contains_pattern("100%_a\\b"), "%100\\%\\_a\\\\b%");
    }

    #[test]
    fn attach_statements_quote_the_path() {
        assert_eq!(
            attach_read_only(Path::new("/data/it's/search.sqlite"), "search"),
            "ATTACH DATABASE 'file:/data/it''s/search.sqlite?mode=ro' AS search;"
        );
    }

    fn indexed(store: &Store) -> Vec<String> {
        let unindexed = store.search_unindexed(100).unwrap();
        let documents: Vec<_> = unindexed
            .iter()
            .map(|blob| {
                let bytes = store.read_blob(blob.hash).unwrap();
                (blob.hash, extract::extract(&blob.path, &bytes))
            })
            .collect();
        store.index_search_documents(&documents).unwrap();
        unindexed.into_iter().map(|blob| blob.path).collect()
    }

    fn found(store: &Store, site: Option<&str>, prefix: &str, expression: &str) -> Vec<String> {
        let (total, hits) = store
            .search_content(site, prefix, expression, 50, 0)
            .unwrap();
        assert_eq!(usize::try_from(total).unwrap(), hits.len());
        hits.into_iter()
            .map(|hit| format!("{}/{}", hit.site, hit.path))
            .collect()
    }

    fn new_store() -> (tempfile::TempDir, Store) {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().to_path_buf()).unwrap();
        (root, store)
    }

    #[test]
    fn the_index_follows_the_live_files() {
        let (_root, store) = new_store();
        store
            .put_file(
                "nlab",
                "kan-extension/index.html",
                b"<title>Kan extension</title><p>why are Kan extensions mentioned</p>",
            )
            .unwrap();
        store
            .put_file(
                "nlab",
                "sheaf.md",
                b"# Sheaf\n\nA sheaf is a presheaf with gluing.\n",
            )
            .unwrap();
        store
            .put_file("other", "notes.txt", b"Notes\n\nkan extensions, again")
            .unwrap();
        store.put_file("other", "logo.png", b"\x89PNG\0\0").unwrap();
        assert_eq!(store.search_pending().unwrap(), 4);
        let mut seen = indexed(&store);
        seen.sort();
        assert_eq!(
            seen,
            [
                "kan-extension/index.html",
                "logo.png",
                "notes.txt",
                "sheaf.md"
            ]
        );
        assert_eq!(store.search_pending().unwrap(), 0);
        assert_eq!(store.search_documents().unwrap(), 3);

        // Stemming: `extension` finds `extensions`; the title hit ranks first.
        assert_eq!(
            found(&store, None, "", "(\"kan\" \"extension\")"),
            ["nlab/kan-extension/index.html", "other/notes.txt"]
        );
        assert_eq!(
            found(&store, None, "", "(\"kan\" \"ext\"*)"),
            ["nlab/kan-extension/index.html", "other/notes.txt"]
        );
        assert_eq!(
            found(&store, Some("other"), "", "(\"kan\")"),
            ["other/notes.txt"]
        );
        assert_eq!(
            found(&store, Some("nlab"), "kan-extension/", "(\"kan\")"),
            ["nlab/kan-extension/index.html"]
        );
        assert!(found(&store, Some("nlab"), "sheaf/", "(\"kan\")").is_empty());
        assert_eq!(
            found(&store, None, "", "(\"kan\") NOT \"mentioned\""),
            ["other/notes.txt"]
        );
        let (_, hits) = store
            .search_content(None, "", "(\"gluing\")", 50, 0)
            .unwrap();
        assert_eq!(hits[0].title, "Sheaf");
        assert_eq!(
            hits[0].snippet.as_deref(),
            Some("Sheaf A sheaf is a presheaf with \u{E000}gluing\u{E001}.")
        );

        // Paging is stable: page two of one-per-page is the second hit.
        let (total, page) = store
            .search_content(None, "", "(\"a\" OR \"kan\" OR \"sheaf\")", 1, 1)
            .unwrap();
        assert_eq!(total, 3);
        assert_eq!(page.len(), 1);

        // An unknown site is not found, like name search.
        assert!(matches!(
            store.search_content(Some("missing"), "", "(\"x\")", 1, 0),
            Err(StoreError::NotFound)
        ));
    }

    #[test]
    fn documents_follow_their_blobs_and_garbage_is_collected() {
        let (_root, store) = new_store();
        store
            .put_file("other", "notes.txt", b"Notes\n\nkan extensions, again")
            .unwrap();
        store.put_file("other", "a.md", b"# A\n\nalpha").unwrap();
        indexed(&store);
        assert_eq!(store.search_documents().unwrap(), 2);

        // The same blob at a second path is a second hit without any
        // indexing; a delete is gone at once, and the document stays for the
        // path that remains.
        store
            .put_file("other", "copy.txt", b"Notes\n\nkan extensions, again")
            .unwrap();
        assert_eq!(store.search_pending().unwrap(), 0);
        assert_eq!(
            found(&store, Some("other"), "", "(\"again\")"),
            ["other/copy.txt", "other/notes.txt"]
        );
        store.delete_file("other", "notes.txt").unwrap();
        assert_eq!(
            found(&store, Some("other"), "", "(\"again\")"),
            ["other/copy.txt"]
        );
        assert_eq!(store.search_collect_garbage().unwrap(), 0);
        store.delete_file("other", "copy.txt").unwrap();
        assert_eq!(store.search_collect_garbage().unwrap(), 1);
        assert_eq!(store.search_documents().unwrap(), 1);
        store.search_optimize().unwrap();
        assert_eq!(found(&store, None, "", "(\"alpha\")"), ["other/a.md"]);
    }

    #[test]
    fn a_format_change_rebuilds_the_index() {
        let (root, store) = new_store();
        store.put_file("s", "a.txt", b"hello there").unwrap();
        indexed(&store);
        assert_eq!(store.search_documents().unwrap(), 1);
        drop(store);
        {
            let path = root.path().join(SEARCH_FILE);
            let mut db = SqliteConnection::establish(&path.to_string_lossy()).unwrap();
            db.batch_execute("PRAGMA user_version = 999;").unwrap();
        }
        let store = Store::new(root.path().to_path_buf()).unwrap();
        assert_eq!(store.search_documents().unwrap(), 0);
        assert_eq!(store.search_pending().unwrap(), 1);
        indexed(&store);
        assert_eq!(store.search_documents().unwrap(), 1);
    }
}
