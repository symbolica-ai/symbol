// One slice of `store`: it shares the parent's imports and types rather
// than re-listing ~90 names. `allow`, not `expect`, because clippy
// reports an expectation on a `use` item as unfulfilled.
#[allow(clippy::wildcard_imports)]
use super::*;

impl UndoKind {
    pub(super) const fn as_str(self) -> &'static str {
        match self {
            Self::Put => "put",
            Self::DeletePath => "delete_path",
            Self::DeleteSite => "delete_site",
            Self::Copy => "copy",
            Self::Move => "move",
            Self::Expiry => "expiry",
            Self::ExpireSweep => "expire_sweep",
            Self::PutFile => "put_file",
            Self::Allocate => "allocate",
            Self::Replace => "replace",
            Self::Splice => "splice",
            Self::Alias => "alias",
        }
    }

    pub(super) const fn from_i64(value: i64) -> Result<Self, StoreError> {
        match value {
            1 => Ok(Self::Put),
            2 => Ok(Self::DeletePath),
            3 => Ok(Self::DeleteSite),
            4 => Ok(Self::Copy),
            5 => Ok(Self::Move),
            6 => Ok(Self::Expiry),
            7 => Ok(Self::ExpireSweep),
            8 => Ok(Self::PutFile),
            9 => Ok(Self::Allocate),
            10 => Ok(Self::Replace),
            11 => Ok(Self::Splice),
            12 => Ok(Self::Alias),
            _ => Err(StoreError::UnsupportedUndoKind(value)),
        }
    }
}

pub(super) struct SiteSnapshot {
    name: String,
    existed: bool,
    public_url: String,
    created: Option<i64>,
    updated: i64,
    content_revision: i64,
    tree_hash: TreeHash,
}

use super::rows::{
    NewAliasEntry, NewAllocatedEntry, NewUndoAliasDelta, NewUndoAllocatedDelta, NewUndoFileDelta,
    NewUndoSite, insert_alias_entry, insert_allocated_entry, insert_file_entry,
    insert_undo_operation, snapshot_file_rows,
};

pub(super) fn snapshot_site(
    tx: &mut SqliteConnection,
    name: &str,
    kind: UndoKind,
    now: i64,
) -> Result<UndoInfo, StoreError> {
    let description = match kind {
        UndoKind::Put | UndoKind::PutFile => format!("restore previous state of {name}"),
        UndoKind::DeletePath | UndoKind::DeleteSite => format!("restore deleted site {name}"),
        UndoKind::Copy => format!("remove copied site {name}"),
        UndoKind::Move => format!("restore previous name {name}"),
        UndoKind::Expiry => format!("restore previous expiry policy for {name}"),
        UndoKind::ExpireSweep => format!("restore expired content in {name}"),
        UndoKind::Allocate => format!("remove allocated content from {name}"),
        UndoKind::Replace => format!("restore replaced content in {name}"),
        UndoKind::Splice => format!("restore spliced content in {name}"),
        UndoKind::Alias => format!("restore aliases in {name}"),
    };
    snapshot_site_with_description(tx, name, kind, &description, now)
}

/// Records an allocated entry's state before a change. `entry` is its metadata
/// and `modified` time, or `None` when the path did not exist.
pub(super) fn insert_undo_allocated(
    tx: &mut SqliteConnection,
    token: &str,
    path: &str,
    entry: Option<(&AllocatedMetadata, i64)>,
) -> Result<(), diesel::result::Error> {
    let metadata = entry.map(|(metadata, _)| metadata);
    diesel::insert_into(undo_allocated_deltas::table)
        .values(NewUndoAllocatedDelta {
            token,
            path,
            existed: i64::from(metadata.is_some()),
            hash: metadata.map(|value| value.hash),
            size: metadata.map(|value| value.size),
            naming_mode: metadata.map(|value| value.naming_mode as i64),
            prefix: metadata.map(|value| value.prefix.clone()),
            suffix: metadata.map(|value| value.suffix.clone()),
            extension: metadata.and_then(|value| value.extension.clone()),
            media_type: metadata.map(|value| value.media_type.clone()),
            modified: entry.map(|(_, modified)| modified),
        })
        .execute(tx)?;
    Ok(())
}

pub(super) fn insert_undo_alias(
    tx: &mut SqliteConnection,
    token: &str,
    path: &str,
    alias: Option<&AliasRow>,
) -> Result<(), diesel::result::Error> {
    diesel::insert_into(undo_alias_deltas::table)
        .values(NewUndoAliasDelta {
            token,
            path,
            existed: i64::from(alias.is_some()),
            canonical_target: alias.map(|row| row.canonical_target.clone()),
            resolved_kind: alias.and_then(|row| row.resolved_kind),
            resolved_hash: alias.and_then(|row| row.resolved_hash),
            resolved_size: alias.and_then(|row| row.resolved_size),
            modified: alias.map(|row| row.modified),
        })
        .execute(tx)?;
    Ok(())
}

/// Records a file's state before a change. `entry` is its hash, size and
/// `modified` time, or `None` when the path held no file.
pub(super) fn insert_undo_file_delta(
    tx: &mut SqliteConnection,
    token: &str,
    path: &str,
    entry: Option<(ContentHash, i64, i64)>,
) -> Result<(), diesel::result::Error> {
    diesel::insert_into(undo_file_deltas::table)
        .values(NewUndoFileDelta::new(token, path, entry))
        .execute(tx)?;
    Ok(())
}

#[expect(clippy::too_many_lines)]
pub(super) fn snapshot_site_with_description(
    tx: &mut SqliteConnection,
    name: &str,
    kind: UndoKind,
    description: &str,
    now: i64,
) -> Result<UndoInfo, StoreError> {
    let token = undo_token()?;
    let expires = insert_undo_operation(tx, &token, kind, description, name, now)?;
    let site = sites::table
        .filter(sites::name.eq(name))
        .select((
            sites::name,
            sites::public_url,
            sites::created,
            sites::updated,
            sites::content_revision,
            sites::tree_hash,
        ))
        .first::<(String, String, Option<i64>, i64, i64, TreeHash)>(tx)
        .optional()?
        .map_or_else(
            || SiteSnapshot {
                name: name.to_string(),
                existed: false,
                public_url: String::new(),
                created: None,
                updated: now,
                content_revision: 0,
                tree_hash: TreeHash::EMPTY,
            },
            |(name, public_url, created, updated, content_revision, tree_hash)| SiteSnapshot {
                name,
                existed: true,
                public_url,
                created,
                updated,
                content_revision,
                tree_hash,
            },
        );
    diesel::insert_into(undo_sites::table)
        .values(NewUndoSite {
            token: &token,
            name: &site.name,
            existed: i64::from(site.existed),
            public_url: &site.public_url,
            created: site.created,
            updated: site.updated,
            content_revision: site.content_revision,
            tree_hash: site.tree_hash,
        })
        .execute(tx)?;
    if site.existed {
        let site_id = site_id_locked(tx, name)?;
        snapshot_file_rows(tx, &token, site_id)?;
        let saved_allocated = allocated_entries::table
            .filter(allocated_entries::site_id.eq(site_id))
            .select((
                allocated_entries::path,
                allocated_entries::hash,
                allocated_entries::size,
                allocated_entries::naming_mode,
                allocated_entries::prefix,
                allocated_entries::suffix,
                allocated_entries::extension,
                allocated_entries::media_type,
                allocated_entries::modified,
            ))
            .load::<(
                String,
                ContentHash,
                i64,
                i64,
                String,
                String,
                Option<String>,
                String,
                i64,
            )>(tx)?;
        for (path, hash, size, naming_mode, prefix, suffix, extension, media_type, modified) in
            saved_allocated
        {
            insert_undo_allocated(
                tx,
                &token,
                &path,
                Some((
                    &AllocatedMetadata {
                        hash,
                        size,
                        naming_mode: AllocatedNamingMode::try_from(naming_mode)?,
                        prefix,
                        suffix,
                        extension,
                        media_type,
                    },
                    modified,
                )),
            )?;
        }
        let saved_aliases = aliases::table
            .filter(aliases::site_id.eq(site_id))
            .select(AliasRow::as_select())
            .load::<AliasRow>(tx)?;
        for alias in saved_aliases {
            insert_undo_alias(tx, &token, &alias.path, Some(&alias))?;
        }
        snapshot_expiry_policies_locked(tx, &token, site_id)?;
    }
    Ok(UndoInfo {
        token,
        expires_at: format_timestamp(expires),
    })
}

pub(super) fn snapshot_entry_deltas(
    tx: &mut SqliteConnection,
    name: &str,
    kind: UndoKind,
    description: &str,
    paths: &[&str],
    now: i64,
) -> Result<UndoInfo, StoreError> {
    let token = undo_token()?;
    let expires = insert_undo_operation(tx, &token, kind, description, name, now)?;
    let (site_id, public_url, created, updated, content_revision, tree_hash) = sites::table
        .filter(sites::name.eq(name))
        .select((
            sites::id,
            sites::public_url,
            sites::created,
            sites::updated,
            sites::content_revision,
            sites::tree_hash,
        ))
        .first::<(i64, String, Option<i64>, i64, i64, TreeHash)>(tx)?;
    diesel::insert_into(undo_sites::table)
        .values(NewUndoSite {
            token: &token,
            name,
            existed: 1,
            public_url: &public_url,
            created,
            updated,
            content_revision,
            tree_hash,
        })
        .execute(tx)?;
    if matches!(kind, UndoKind::Alias) {
        let mut previous = HashMap::new();
        for chunk in paths.chunks(SQLITE_DELETE_BATCH_SIZE) {
            previous.extend(
                aliases::table
                    .filter(aliases::site_id.eq(site_id))
                    .filter(aliases::path.eq_any(chunk))
                    .select(AliasRow::as_select())
                    .load::<AliasRow>(tx)?
                    .into_iter()
                    .map(|row| (row.path.clone(), row)),
            );
        }
        for path in paths {
            insert_undo_alias(tx, &token, path, previous.get(*path))?;
        }
        snapshot_expiry_policies_locked(tx, &token, site_id)?;
        return Ok(UndoInfo {
            token,
            expires_at: format_timestamp(expires),
        });
    }
    let operation_is_allocated = matches!(kind, UndoKind::Allocate)
        || (matches!(kind, UndoKind::Replace | UndoKind::Splice)
            && site_entries::table
                .filter(site_entries::site_id.eq(site_id))
                .filter(site_entries::path.eq_any(paths))
                .filter(site_entries::kind.eq(database::schema::ALLOCATED_ENTRY_KIND))
                .select(count_star())
                .first::<i64>(tx)?
                > 0);
    for path in paths {
        let entry_kind = site_entries::table
            .find((site_id, *path))
            .select(site_entries::kind)
            .first::<i64>(tx)
            .optional()?;
        match entry_kind {
            Some(entry_kind) if entry_kind == database::schema::FILE_ENTRY_KIND => {
                let (hash, size, modified) = files::table
                    .find((site_id, *path))
                    .select((files::hash, files::size, files::modified))
                    .first::<(ContentHash, i64, i64)>(tx)?;
                insert_undo_file_delta(tx, &token, path, Some((hash, size, modified)))?;
            }
            Some(entry_kind) if entry_kind == database::schema::ALLOCATED_ENTRY_KIND => {
                let metadata = allocated_metadata_locked(tx, site_id, path)?;
                let modified = allocated_entries::table
                    .find((site_id, *path))
                    .select(allocated_entries::modified)
                    .first::<i64>(tx)?;
                insert_undo_allocated(tx, &token, path, Some((&metadata, modified)))?;
            }
            Some(entry_kind) if entry_kind == database::schema::ALIAS_ENTRY_KIND => {
                let alias = aliases::table
                    .find((site_id, *path))
                    .select(AliasRow::as_select())
                    .first::<AliasRow>(tx)?;
                insert_undo_alias(tx, &token, path, Some(&alias))?;
            }
            Some(_) => return Err(StoreError::DestinationConflict),
            None if matches!(kind, UndoKind::Alias) => {
                insert_undo_alias(tx, &token, path, None)?;
            }
            None if operation_is_allocated => {
                insert_undo_allocated(tx, &token, path, None)?;
            }
            None => insert_undo_file_delta(tx, &token, path, None)?,
        }
    }
    snapshot_expiry_policies_locked(tx, &token, site_id)?;
    Ok(UndoInfo {
        token,
        expires_at: format_timestamp(expires),
    })
}

#[expect(clippy::too_many_lines)]
pub(super) fn restore_entry_deltas(
    tx: &mut SqliteConnection,
    blob_files: &BlobFiles,
    name: &str,
    token: &str,
) -> Result<i64, StoreError> {
    let (updated, content_revision) = undo_sites::table
        .find(token)
        .select((undo_sites::updated, undo_sites::content_revision))
        .first::<(i64, i64)>(tx)?;
    let site_id = site_id_locked(tx, name)?;
    let file_deltas = undo_file_deltas::table
        .filter(undo_file_deltas::token.eq(token))
        .select((
            undo_file_deltas::path,
            undo_file_deltas::existed,
            undo_file_deltas::hash,
            undo_file_deltas::size,
            undo_file_deltas::modified,
        ))
        .load::<(String, i64, Option<ContentHash>, Option<i64>, Option<i64>)>(tx)?;
    let allocated_deltas = undo_allocated_deltas::table
        .filter(undo_allocated_deltas::token.eq(token))
        .select(UndoAllocatedMetadata::as_select())
        .load::<UndoAllocatedMetadata>(tx)?;
    let alias_deltas = undo_alias_deltas::table
        .filter(undo_alias_deltas::token.eq(token))
        .select(UndoAliasRow::as_select())
        .load::<UndoAliasRow>(tx)?;
    // Snapshots taken before entries recorded `modified` restore the latest
    // time their content can have changed: the site's own `updated` then.
    let restored_modified = |modified: Option<i64>| modified.unwrap_or(updated);
    let entry_change_paths = file_deltas
        .iter()
        .map(|row| row.0.clone())
        .chain(allocated_deltas.iter().map(|row| row.path.clone()))
        .collect::<Vec<_>>();
    let alias_change_paths = alias_deltas
        .iter()
        .map(|row| row.path.clone())
        .collect::<Vec<_>>();
    let changed_paths = entry_change_paths
        .iter()
        .chain(&alias_change_paths)
        .map(String::as_str)
        .collect::<Vec<_>>();
    for paths in changed_paths.chunks(512) {
        diesel::delete(
            site_entries::table
                .filter(site_entries::site_id.eq(site_id))
                .filter(site_entries::path.eq_any(paths)),
        )
        .execute(tx)?;
    }
    for (path, existed, hash, size, modified) in file_deltas {
        if existed == 0 {
            continue;
        }
        let hash = hash.expect("existing file delta has hash");
        let size = size.expect("existing file delta has size");
        insert_file_entry(
            tx,
            NewFile {
                site_id,
                path,
                hash,
                size,
                modified: restored_modified(modified),
            },
        )?;
    }
    for delta in allocated_deltas {
        if delta.existed == 0 {
            continue;
        }
        insert_allocated_entry(
            tx,
            NewAllocatedEntry {
                site_id,
                path: &delta.path,
                hash: delta.hash.expect("existing allocated delta has hash"),
                size: delta.size.expect("existing allocated delta has size"),
                naming_mode: delta
                    .naming_mode
                    .expect("existing allocated delta has naming mode"),
                prefix: delta.prefix.expect("existing allocated delta has prefix"),
                suffix: delta.suffix.expect("existing allocated delta has suffix"),
                extension: delta.extension,
                media_type: delta
                    .media_type
                    .expect("existing allocated delta has media type"),
                modified: restored_modified(delta.modified),
            },
        )?;
    }
    for delta in alias_deltas {
        if delta.existed == 0 {
            continue;
        }
        insert_alias_entry(
            tx,
            NewAliasEntry {
                site_id,
                path: &delta.path,
                canonical_target: delta
                    .canonical_target
                    .as_deref()
                    .expect("existing alias delta has target"),
                resolved_kind: delta.resolved_kind,
                resolved_hash: delta.resolved_hash,
                resolved_size: delta.resolved_size,
                modified: restored_modified(delta.modified),
            },
        )?;
    }
    diesel::delete(expiry_policies::table.filter(expiry_policies::site_id.eq(site_id)))
        .execute(tx)?;
    restore_expiry_policies_locked(tx, token, site_id)?;
    rebuild_aggregates_locked(tx, site_id)?;
    diesel::update(sites::table.find(site_id))
        .set(sites::content_revision.eq(content_revision))
        .execute(tx)?;
    let alias_changes = entry_change_paths
        .iter()
        .map(|path| AliasChange::Entry(path))
        .chain(
            alias_change_paths
                .iter()
                .map(|path| AliasChange::Alias(path)),
        )
        .collect::<Vec<_>>();
    refresh_aliases_locked(tx, site_id, &alias_changes)?;
    regenerate_site(tx, blob_files, site_id, updated)?;
    record_site_event(tx, site_id, StoredSiteEventKind::Restore, 0, updated)?;
    Ok(updated)
}

pub(super) fn retain_management_tombstone(
    tx: &mut SqliteConnection,
    name: &str,
    now: i64,
) -> Result<(), diesel::result::Error> {
    let hash = sites::table
        .filter(sites::name.eq(name))
        .filter(sites::management_status.eq(1_i64))
        .select(sites::management_hash)
        .first::<Option<Vec<u8>>>(tx)
        .optional()?
        .flatten();
    if let Some(hash) = hash {
        diesel::insert_into(management_tombstones::table)
            .values((
                management_tombstones::name.eq(name),
                management_tombstones::management_hash.eq(hash),
                management_tombstones::created.eq(now),
            ))
            .on_conflict(management_tombstones::name)
            .do_update()
            .set((
                management_tombstones::management_hash
                    .eq(excluded(management_tombstones::management_hash)),
                management_tombstones::created.eq(excluded(management_tombstones::created)),
            ))
            .execute(tx)?;
    }
    Ok(())
}

pub(super) fn prune_undo_locked(
    tx: &mut SqliteConnection,
    now: i64,
) -> Result<(), diesel::result::Error> {
    diesel::delete(
        undo_operations::table.filter(
            undo_operations::consumed
                .eq(1_i64)
                .or(undo_operations::expires.le(now)),
        ),
    )
    .execute(tx)?;
    let rows = undo_names::table
        .inner_join(undo_operations::table.on(undo_operations::token.eq(undo_names::token)))
        .select((undo_names::name, undo_names::token))
        .order((
            undo_names::name,
            undo_operations::created.desc(),
            undo_operations::rowid.desc(),
        ))
        .load::<(String, String)>(tx)?;
    let mut previous = None::<String>;
    let mut position = 0_i64;
    let mut stale = Vec::new();
    for (name, token) in rows {
        if previous.as_deref() == Some(name.as_str()) {
            position += 1;
        } else {
            previous = Some(name);
            position = 1;
        }
        if position > UNDO_LIMIT_PER_SITE {
            stale.push(token);
        }
    }
    stale.sort_unstable();
    stale.dedup();
    for tokens in stale.chunks(SQLITE_DELETE_BATCH_SIZE) {
        diesel::delete(undo_operations::table.filter(undo_operations::token.eq_any(tokens)))
            .execute(tx)?;
    }
    Ok(())
}

pub(super) fn undo_token() -> Result<String, StoreError> {
    let mut bytes = [0_u8; 16];
    getrandom::fill(&mut bytes).map_err(io::Error::other)?;
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(token, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(token)
}

impl Store {
    pub fn undo_stack(&self, name: &str) -> Result<UndoStack, StoreError> {
        let name = parse_site_name(name)?;
        let now = self.now_millis();
        let mut db = self.inner.readers.get();
        let rows = undo_operations::table
            .inner_join(undo_names::table.on(undo_names::token.eq(undo_operations::token)))
            .filter(undo_names::name.eq(name))
            .filter(undo_operations::consumed.eq(0_i64))
            .filter(undo_operations::expires.gt(now))
            .select((
                undo_operations::token,
                undo_operations::kind,
                undo_operations::description,
                undo_operations::created,
                undo_operations::expires,
            ))
            .order((
                undo_operations::created.desc(),
                undo_operations::rowid.desc(),
            ))
            .load::<(String, i64, String, i64, i64)>(&mut *db)?;
        let entries = rows
            .into_iter()
            .map(|(token, kind, description, created, expires)| {
                Ok(UndoEntry {
                    token,
                    kind: UndoKind::from_i64(kind)?.as_str().to_string(),
                    description,
                    created_at: format_timestamp(created),
                    expires_at: format_timestamp(expires),
                    remaining_seconds: u64::try_from((expires - now).max(0) / 1000)
                        .expect("remaining time is non-negative"),
                })
            })
            .collect::<Result<Vec<_>, StoreError>>()?;
        Ok(UndoStack {
            site: name.to_string(),
            entries,
        })
    }

    pub fn undo_secured(
        &self,
        name: &str,
        guard: Option<&str>,
        authorization: Option<&ManagementToken>,
    ) -> Result<UndoResult, StoreError> {
        let name = parse_site_name(name)?;
        let now = self.now_millis();
        self.write(|tx| self.undo_locked(tx, name, guard, authorization, now))
    }

    #[expect(clippy::too_many_lines)]
    fn undo_locked(
        &self,
        tx: &mut SqliteConnection,
        name: &str,
        guard: Option<&str>,
        authorization: Option<&ManagementToken>,
        now: i64,
    ) -> Result<(UndoResult, Vec<ContentHash>), StoreError> {
        authorize_locked(tx, name, authorization)?;
        let latest = undo_operations::table
            .inner_join(undo_names::table.on(undo_names::token.eq(undo_operations::token)))
            .filter(undo_names::name.eq(name))
            .filter(undo_operations::consumed.eq(0_i64))
            .filter(undo_operations::expires.gt(now))
            .select(undo_operations::token)
            .order((
                undo_operations::created.desc(),
                undo_operations::rowid.desc(),
            ))
            .first::<String>(&mut *tx)
            .optional()?
            .ok_or(StoreError::NotFound)?;
        if guard.is_some_and(|token| token != latest) {
            return Err(StoreError::StaleUndo(latest.into()));
        }
        let latest_kind = UndoKind::from_i64(
            undo_operations::table
                .find(&latest)
                .select(undo_operations::kind)
                .first::<i64>(&mut *tx)?,
        )?;
        let has_deltas = matches!(
            latest_kind,
            UndoKind::Put
                | UndoKind::PutFile
                | UndoKind::DeletePath
                | UndoKind::Allocate
                | UndoKind::Replace
                | UndoKind::Splice
                | UndoKind::Alias
        ) && (undo_file_deltas::table
            .filter(undo_file_deltas::token.eq(&latest))
            .select(count_star())
            .first::<i64>(&mut *tx)?
            + undo_allocated_deltas::table
                .filter(undo_allocated_deltas::token.eq(&latest))
                .select(count_star())
                .first::<i64>(&mut *tx)?
            + undo_alias_deltas::table
                .filter(undo_alias_deltas::token.eq(&latest))
                .select(count_star())
                .first::<i64>(&mut *tx)?
            > 0);
        if has_deltas {
            let restored = restore_entry_deltas(tx, &self.inner.blob_files, name, &latest)?;
            diesel::update(undo_operations::table.find(&latest))
                .set(undo_operations::consumed.eq(1_i64))
                .execute(&mut *tx)?;
            let removed = finish_mutation(tx, now)?;
            return Ok((
                UndoResult {
                    restored_at: format_timestamp(restored),
                },
                removed,
            ));
        }
        let (snapshot_name, existed, public_url, created, updated, content_revision, tree_hash) =
            undo_sites::table
                .find(&latest)
                .select((
                    undo_sites::name,
                    undo_sites::existed,
                    undo_sites::public_url,
                    undo_sites::created,
                    undo_sites::updated,
                    undo_sites::content_revision,
                    undo_sites::tree_hash,
                ))
                .first::<(String, i64, String, Option<i64>, i64, i64, TreeHash)>(&mut *tx)?;
        let snapshot = SiteSnapshot {
            name: snapshot_name,
            existed: existed != 0,
            public_url,
            created,
            updated,
            content_revision,
            tree_hash,
        };
        let names = undo_names::table
            .filter(undo_names::token.eq(&latest))
            .select(undo_names::name)
            .load::<String>(&mut *tx)?;
        for name in names {
            retain_management_tombstone(tx, &name, now)?;
            diesel::delete(sites::table.filter(sites::name.eq(name))).execute(&mut *tx)?;
        }
        if snapshot.existed {
            let site_id = diesel::insert_into(sites::table)
                .values(NewSite {
                    name: &snapshot.name,
                    created: snapshot.created,
                    updated: snapshot.updated,
                    public_url: &snapshot.public_url,
                    content_revision: snapshot.content_revision,
                    tree_hash: snapshot.tree_hash,
                    creator_kind: None,
                    creator_hash: None,
                    claim_hash: None,
                    management_hash: None,
                    management_status: 0,
                })
                .returning(sites::id)
                .get_result::<i64>(&mut *tx)?;
            let retained_hash = management_tombstones::table
                .inner_join(undo_names::table.on(undo_names::name.eq(management_tombstones::name)))
                .filter(undo_names::token.eq(&latest))
                .select(management_tombstones::management_hash)
                .first::<Vec<u8>>(&mut *tx)
                .optional()?;
            if let Some(hash) = retained_hash {
                diesel::update(sites::table.find(site_id))
                    .set((
                        sites::management_hash.eq(Some(hash)),
                        sites::management_status.eq(1_i64),
                    ))
                    .execute(&mut *tx)?;
            }
            // As in `restore_entry_deltas`: a snapshot from before entries
            // recorded `modified` falls back to the site's `updated` then.
            let restored_modified = |modified: Option<i64>| modified.unwrap_or(snapshot.updated);
            let saved_files = undo_files::table
                .filter(undo_files::token.eq(&latest))
                .select((
                    undo_files::path,
                    undo_files::hash,
                    undo_files::size,
                    undo_files::modified,
                ))
                .load::<(String, ContentHash, i64, Option<i64>)>(&mut *tx)?;
            for (path, hash, size, modified) in saved_files {
                insert_file_entry(
                    tx,
                    NewFile {
                        site_id,
                        path,
                        hash,
                        size,
                        modified: restored_modified(modified),
                    },
                )?;
            }
            let saved_allocated = undo_allocated_deltas::table
                .filter(undo_allocated_deltas::token.eq(&latest))
                .filter(undo_allocated_deltas::existed.eq(1_i64))
                .select((
                    undo_allocated_deltas::path,
                    undo_allocated_deltas::hash,
                    undo_allocated_deltas::size,
                    undo_allocated_deltas::naming_mode,
                    undo_allocated_deltas::prefix,
                    undo_allocated_deltas::suffix,
                    undo_allocated_deltas::extension,
                    undo_allocated_deltas::media_type,
                    undo_allocated_deltas::modified,
                ))
                .load::<(
                    String,
                    Option<ContentHash>,
                    Option<i64>,
                    Option<i64>,
                    Option<String>,
                    Option<String>,
                    Option<String>,
                    Option<String>,
                    Option<i64>,
                )>(&mut *tx)?;
            for (path, hash, size, naming_mode, prefix, suffix, extension, media_type, modified) in
                saved_allocated
            {
                insert_allocated_entry(
                    tx,
                    NewAllocatedEntry {
                        site_id,
                        path: &path,
                        hash: hash.expect("whole-site allocated snapshot has hash"),
                        size: size.expect("whole-site allocated snapshot has size"),
                        naming_mode: naming_mode
                            .expect("whole-site allocated snapshot has naming mode"),
                        prefix: prefix.expect("whole-site allocated snapshot has prefix"),
                        suffix: suffix.expect("whole-site allocated snapshot has suffix"),
                        extension,
                        media_type: media_type
                            .expect("whole-site allocated snapshot has media type"),
                        modified: restored_modified(modified),
                    },
                )?;
            }
            let saved_aliases = undo_alias_deltas::table
                .filter(undo_alias_deltas::token.eq(&latest))
                .filter(undo_alias_deltas::existed.eq(1_i64))
                .select(UndoAliasRow::as_select())
                .load::<UndoAliasRow>(&mut *tx)?;
            for alias in saved_aliases {
                insert_alias_entry(
                    tx,
                    NewAliasEntry {
                        site_id,
                        path: &alias.path,
                        canonical_target: alias
                            .canonical_target
                            .as_deref()
                            .expect("whole-site alias snapshot has target"),
                        resolved_kind: alias.resolved_kind,
                        resolved_hash: alias.resolved_hash,
                        resolved_size: alias.resolved_size,
                        modified: restored_modified(alias.modified),
                    },
                )?;
            }
            restore_expiry_policies_locked(tx, &latest, site_id)?;
            rebuild_aggregates_locked(tx, site_id)?;
            regenerate_site(tx, &self.inner.blob_files, site_id, snapshot.updated)?;
            record_site_event(
                tx,
                site_id,
                StoredSiteEventKind::Restore,
                0,
                snapshot.updated,
            )?;
            let tombstone_names = undo_names::table
                .filter(undo_names::token.eq(&latest))
                .select(undo_names::name)
                .load::<String>(&mut *tx)?;
            for names in tombstone_names.chunks(SQLITE_DELETE_BATCH_SIZE) {
                diesel::delete(
                    management_tombstones::table.filter(management_tombstones::name.eq_any(names)),
                )
                .execute(&mut *tx)?;
            }
        }
        diesel::update(undo_operations::table.find(&latest))
            .set(undo_operations::consumed.eq(1_i64))
            .execute(&mut *tx)?;
        let removed = finish_mutation(tx, now)?;
        Ok((
            UndoResult {
                restored_at: format_timestamp(snapshot.updated),
            },
            removed,
        ))
    }
}
