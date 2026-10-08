// Insertable rows and the small helpers that write them. Every struct sets
// `treat_none_as_default_value = false` so a `None` is bound as NULL, exactly
// like `column.eq(Option::<T>::None)` is; Diesel's default would instead omit
// the column and take its table default.

use diesel::prelude::*;
use diesel::sql_types::{BigInt, Text};
use diesel::sqlite::SqliteConnection;

use super::{
    ContentHash, MANIFEST_PATH, TreeHash, UNDO_RETENTION_MILLIS, UndoKind, ensure_file_entry,
};
use crate::database::schema::{ALIAS_ENTRY_KIND, ALLOCATED_ENTRY_KIND, FILE_ENTRY_KIND};
use crate::schema::{
    aliases, allocated_entries, files, path_aggregates, site_entries, undo_alias_deltas,
    undo_allocated_deltas, undo_file_deltas, undo_files, undo_names, undo_operations, undo_sites,
};

/// Inserts the `site_entries` row every typed entry hangs off. A plain insert:
/// an existing `(site_id, path)` is an error, not a no-op.
pub(super) fn insert_entry(
    tx: &mut SqliteConnection,
    site_id: i64,
    path: &str,
    kind: i64,
) -> QueryResult<()> {
    diesel::insert_into(site_entries::table)
        .values((
            site_entries::site_id.eq(site_id),
            site_entries::path.eq(path),
            site_entries::kind.eq(kind),
        ))
        .execute(tx)?;
    Ok(())
}

/// A row of `allocated_entries`; `kind` is filled in by [`insert_allocated_entry`].
#[derive(Insertable)]
#[diesel(table_name = allocated_entries, treat_none_as_default_value = false)]
pub(super) struct NewAllocatedEntry<'a> {
    pub(super) site_id: i64,
    pub(super) path: &'a str,
    pub(super) hash: ContentHash,
    pub(super) size: i64,
    pub(super) naming_mode: i64,
    pub(super) prefix: String,
    pub(super) suffix: String,
    pub(super) extension: Option<String>,
    pub(super) media_type: String,
    pub(super) modified: i64,
}

/// A row of `aliases`; `kind` is filled in by [`insert_alias_entry`].
#[derive(Insertable)]
#[diesel(table_name = aliases, treat_none_as_default_value = false)]
pub(super) struct NewAliasEntry<'a> {
    pub(super) site_id: i64,
    pub(super) path: &'a str,
    pub(super) canonical_target: &'a str,
    pub(super) resolved_kind: Option<i64>,
    pub(super) resolved_hash: Option<ContentHash>,
    pub(super) resolved_size: Option<i64>,
    pub(super) modified: i64,
}

/// Inserts a file's `site_entries` row (if it has none yet) and its `files` row.
pub(super) fn insert_file_entry(
    tx: &mut SqliteConnection,
    file: super::NewFile,
) -> QueryResult<()> {
    ensure_file_entry(tx, file.site_id, &file.path)?;
    diesel::insert_into(files::table).values(file).execute(tx)?;
    Ok(())
}

/// Inserts an allocated entry's `site_entries` row and its `allocated_entries` row.
pub(super) fn insert_allocated_entry(
    tx: &mut SqliteConnection,
    entry: NewAllocatedEntry<'_>,
) -> QueryResult<()> {
    insert_entry(tx, entry.site_id, entry.path, ALLOCATED_ENTRY_KIND)?;
    diesel::insert_into(allocated_entries::table)
        .values((entry, allocated_entries::kind.eq(ALLOCATED_ENTRY_KIND)))
        .execute(tx)?;
    Ok(())
}

/// Inserts an alias's `site_entries` row and its `aliases` row.
pub(super) fn insert_alias_entry(
    tx: &mut SqliteConnection,
    alias: NewAliasEntry<'_>,
) -> QueryResult<()> {
    insert_entry(tx, alias.site_id, alias.path, ALIAS_ENTRY_KIND)?;
    diesel::insert_into(aliases::table)
        .values((alias, aliases::kind.eq(ALIAS_ENTRY_KIND)))
        .execute(tx)?;
    Ok(())
}

/// Copies every entry of one site into another, empty one, inside the
/// database: files (but not the manifest, which is regenerated for the
/// destination), allocated entries, aliases and path aggregates. Values,
/// including each entry's `modified`, are copied column for column and never
/// pass through Rust. The `site_entries` rows go first because every typed
/// row references one.
pub(super) fn copy_site_entries(
    tx: &mut SqliteConnection,
    source_id: i64,
    destination_id: i64,
) -> QueryResult<()> {
    copy_entry_rows(tx, source_id, destination_id)?;
    copy_file_rows(tx, source_id, destination_id)?;
    copy_allocated_rows(tx, source_id, destination_id)?;
    copy_alias_rows(tx, source_id, destination_id)?;
    copy_aggregate_rows(tx, source_id, destination_id)
}

fn copy_entry_rows(
    tx: &mut SqliteConnection,
    source_id: i64,
    destination_id: i64,
) -> QueryResult<()> {
    let destination = destination_id.into_sql::<BigInt>();
    let columns = (
        site_entries::site_id,
        site_entries::path,
        site_entries::kind,
    );
    diesel::insert_into(site_entries::table)
        .values(
            files::table
                .filter(files::site_id.eq(source_id))
                .filter(files::path.ne(MANIFEST_PATH))
                .select((destination, files::path, files::kind)),
        )
        .into_columns(columns)
        .execute(tx)?;
    diesel::insert_into(site_entries::table)
        .values(
            allocated_entries::table
                .filter(allocated_entries::site_id.eq(source_id))
                .select((
                    destination,
                    allocated_entries::path,
                    allocated_entries::kind,
                )),
        )
        .into_columns(columns)
        .execute(tx)?;
    diesel::insert_into(site_entries::table)
        .values(
            aliases::table
                .filter(aliases::site_id.eq(source_id))
                .select((destination, aliases::path, aliases::kind)),
        )
        .into_columns(columns)
        .execute(tx)?;
    Ok(())
}

fn copy_file_rows(
    tx: &mut SqliteConnection,
    source_id: i64,
    destination_id: i64,
) -> QueryResult<()> {
    diesel::insert_into(files::table)
        .values(
            files::table
                .filter(files::site_id.eq(source_id))
                .filter(files::path.ne(MANIFEST_PATH))
                .select((
                    destination_id.into_sql::<BigInt>(),
                    files::path,
                    files::hash,
                    files::size,
                    files::modified,
                )),
        )
        .into_columns((
            files::site_id,
            files::path,
            files::hash,
            files::size,
            files::modified,
        ))
        .execute(tx)?;
    Ok(())
}

fn copy_allocated_rows(
    tx: &mut SqliteConnection,
    source_id: i64,
    destination_id: i64,
) -> QueryResult<()> {
    diesel::insert_into(allocated_entries::table)
        .values(
            allocated_entries::table
                .filter(allocated_entries::site_id.eq(source_id))
                .select((
                    destination_id.into_sql::<BigInt>(),
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
                )),
        )
        .into_columns((
            allocated_entries::site_id,
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
        .execute(tx)?;
    Ok(())
}

fn copy_alias_rows(
    tx: &mut SqliteConnection,
    source_id: i64,
    destination_id: i64,
) -> QueryResult<()> {
    diesel::insert_into(aliases::table)
        .values(
            aliases::table
                .filter(aliases::site_id.eq(source_id))
                .select((
                    destination_id.into_sql::<BigInt>(),
                    aliases::path,
                    aliases::kind,
                    aliases::canonical_target,
                    aliases::resolved_kind,
                    aliases::resolved_hash,
                    aliases::resolved_size,
                    aliases::modified,
                )),
        )
        .into_columns((
            aliases::site_id,
            aliases::path,
            aliases::kind,
            aliases::canonical_target,
            aliases::resolved_kind,
            aliases::resolved_hash,
            aliases::resolved_size,
            aliases::modified,
        ))
        .execute(tx)?;
    Ok(())
}

fn copy_aggregate_rows(
    tx: &mut SqliteConnection,
    source_id: i64,
    destination_id: i64,
) -> QueryResult<()> {
    diesel::insert_into(path_aggregates::table)
        .values(
            path_aggregates::table
                .filter(path_aggregates::site_id.eq(source_id))
                .select((
                    destination_id.into_sql::<BigInt>(),
                    path_aggregates::path,
                    path_aggregates::logical_bytes,
                    path_aggregates::file_count,
                )),
        )
        .into_columns((
            path_aggregates::site_id,
            path_aggregates::path,
            path_aggregates::logical_bytes,
            path_aggregates::file_count,
        ))
        .execute(tx)?;
    Ok(())
}

#[derive(Insertable)]
#[diesel(table_name = undo_operations, treat_none_as_default_value = false)]
pub(super) struct NewUndoOperation<'a> {
    pub(super) token: &'a str,
    pub(super) kind: i64,
    pub(super) description: &'a str,
    pub(super) created: i64,
    pub(super) expires: i64,
    pub(super) consumed: i64,
}

/// Opens an undo record for `name` created at `created`: its
/// `undo_operations` row (unconsumed) and the `undo_names` row that files it
/// under the site. Returns when the record expires.
pub(super) fn insert_undo_operation(
    tx: &mut SqliteConnection,
    token: &str,
    kind: UndoKind,
    description: &str,
    name: &str,
    created: i64,
) -> QueryResult<i64> {
    let expires = created + UNDO_RETENTION_MILLIS;
    diesel::insert_into(undo_operations::table)
        .values(NewUndoOperation {
            token,
            kind: kind as i64,
            description,
            created,
            expires,
            consumed: 0,
        })
        .execute(tx)?;
    diesel::insert_into(undo_names::table)
        .values((undo_names::token.eq(token), undo_names::name.eq(name)))
        .execute(tx)?;
    Ok(expires)
}

#[derive(Insertable)]
#[diesel(table_name = undo_sites, treat_none_as_default_value = false)]
pub(super) struct NewUndoSite<'a> {
    pub(super) token: &'a str,
    pub(super) name: &'a str,
    pub(super) existed: i64,
    pub(super) public_url: &'a str,
    pub(super) created: Option<i64>,
    pub(super) updated: i64,
    pub(super) content_revision: i64,
    pub(super) tree_hash: TreeHash,
}

/// Saves every file of the site except the manifest (regenerated on restore)
/// under `token`, inside the database.
pub(super) fn snapshot_file_rows(
    tx: &mut SqliteConnection,
    token: &str,
    site_id: i64,
) -> QueryResult<()> {
    diesel::insert_into(undo_files::table)
        .values(
            files::table
                .filter(files::site_id.eq(site_id))
                .filter(files::path.ne(MANIFEST_PATH))
                .select((
                    token.into_sql::<Text>(),
                    files::path,
                    files::hash,
                    files::size,
                    files::modified.nullable(),
                )),
        )
        .into_columns((
            undo_files::token,
            undo_files::path,
            undo_files::hash,
            undo_files::size,
            undo_files::modified,
        ))
        .execute(tx)?;
    Ok(())
}

/// An `undo_file_deltas` row: every column after `existed` is NULL when the
/// path did not exist.
#[derive(Insertable)]
#[diesel(table_name = undo_file_deltas, treat_none_as_default_value = false)]
pub(super) struct NewUndoFileDelta<'a> {
    pub(super) token: &'a str,
    pub(super) path: &'a str,
    pub(super) existed: i64,
    pub(super) kind: Option<i64>,
    pub(super) hash: Option<ContentHash>,
    pub(super) size: Option<i64>,
    pub(super) modified: Option<i64>,
}

impl<'a> NewUndoFileDelta<'a> {
    /// `entry` is the file's hash, size and `modified` time, or `None` when
    /// the path held no file.
    pub(super) fn new(
        token: &'a str,
        path: &'a str,
        entry: Option<(ContentHash, i64, i64)>,
    ) -> Self {
        Self {
            token,
            path,
            existed: i64::from(entry.is_some()),
            kind: entry.map(|_| FILE_ENTRY_KIND),
            hash: entry.map(|(hash, _, _)| hash),
            size: entry.map(|(_, size, _)| size),
            modified: entry.map(|(_, _, modified)| modified),
        }
    }
}

#[derive(Insertable)]
#[diesel(table_name = undo_allocated_deltas, treat_none_as_default_value = false)]
pub(super) struct NewUndoAllocatedDelta<'a> {
    pub(super) token: &'a str,
    pub(super) path: &'a str,
    pub(super) existed: i64,
    pub(super) hash: Option<ContentHash>,
    pub(super) size: Option<i64>,
    pub(super) naming_mode: Option<i64>,
    pub(super) prefix: Option<String>,
    pub(super) suffix: Option<String>,
    pub(super) extension: Option<String>,
    pub(super) media_type: Option<String>,
    pub(super) modified: Option<i64>,
}

#[derive(Insertable)]
#[diesel(table_name = undo_alias_deltas, treat_none_as_default_value = false)]
pub(super) struct NewUndoAliasDelta<'a> {
    pub(super) token: &'a str,
    pub(super) path: &'a str,
    pub(super) existed: i64,
    pub(super) canonical_target: Option<String>,
    pub(super) resolved_kind: Option<i64>,
    pub(super) resolved_hash: Option<ContentHash>,
    pub(super) resolved_size: Option<i64>,
    pub(super) modified: Option<i64>,
}

#[cfg(test)]
mod tests {
    use diesel::debug_query;
    use diesel::sqlite::Sqlite;

    use super::*;

    fn hash(label: &str) -> ContentHash {
        ContentHash::from(blake3::hash(label.as_bytes()))
    }

    /// The SQL and bound values a statement would run, for comparing the
    /// struct form of an insert with the column-by-column form it replaced.
    fn rendered<T: diesel::query_builder::QueryFragment<Sqlite>>(statement: &T) -> String {
        format!("{:?}", debug_query::<Sqlite, _>(statement))
    }

    #[test]
    fn undo_delta_rows_bind_the_same_sql_and_values_as_the_explicit_column_form() {
        let missing_file = diesel::insert_into(undo_file_deltas::table)
            .values(NewUndoFileDelta::new("t", "p", None));
        let missing_file_old = diesel::insert_into(undo_file_deltas::table).values((
            undo_file_deltas::token.eq("t"),
            undo_file_deltas::path.eq("p"),
            undo_file_deltas::existed.eq(0_i64),
            undo_file_deltas::kind.eq(Option::<i64>::None),
            undo_file_deltas::hash.eq(Option::<ContentHash>::None),
            undo_file_deltas::size.eq(Option::<i64>::None),
            undo_file_deltas::modified.eq(Option::<i64>::None),
        ));
        assert_eq!(rendered(&missing_file), rendered(&missing_file_old));

        let present_file = diesel::insert_into(undo_file_deltas::table)
            .values(NewUndoFileDelta::new("t", "p", Some((hash("h"), 7, 9))));
        let present_file_old = diesel::insert_into(undo_file_deltas::table).values((
            undo_file_deltas::token.eq("t"),
            undo_file_deltas::path.eq("p"),
            undo_file_deltas::existed.eq(1_i64),
            undo_file_deltas::kind.eq(Some(FILE_ENTRY_KIND)),
            undo_file_deltas::hash.eq(Some(hash("h"))),
            undo_file_deltas::size.eq(Some(7_i64)),
            undo_file_deltas::modified.eq(Some(9_i64)),
        ));
        assert_eq!(rendered(&present_file), rendered(&present_file_old));

        let allocated =
            diesel::insert_into(undo_allocated_deltas::table).values(NewUndoAllocatedDelta {
                token: "t",
                path: "p",
                existed: 0,
                hash: None,
                size: None,
                naming_mode: None,
                prefix: None,
                suffix: None,
                extension: None,
                media_type: None,
                modified: None,
            });
        let allocated_old = diesel::insert_into(undo_allocated_deltas::table).values((
            undo_allocated_deltas::token.eq("t"),
            undo_allocated_deltas::path.eq("p"),
            undo_allocated_deltas::existed.eq(0_i64),
            undo_allocated_deltas::hash.eq(Option::<ContentHash>::None),
            undo_allocated_deltas::size.eq(Option::<i64>::None),
            undo_allocated_deltas::naming_mode.eq(Option::<i64>::None),
            undo_allocated_deltas::prefix.eq(Option::<String>::None),
            undo_allocated_deltas::suffix.eq(Option::<String>::None),
            undo_allocated_deltas::extension.eq(Option::<String>::None),
            undo_allocated_deltas::media_type.eq(Option::<String>::None),
            undo_allocated_deltas::modified.eq(Option::<i64>::None),
        ));
        assert_eq!(rendered(&allocated), rendered(&allocated_old));

        let alias = diesel::insert_into(undo_alias_deltas::table).values(NewUndoAliasDelta {
            token: "t",
            path: "p",
            existed: 0,
            canonical_target: None,
            resolved_kind: None,
            resolved_hash: None,
            resolved_size: None,
            modified: None,
        });
        let alias_old = diesel::insert_into(undo_alias_deltas::table).values((
            undo_alias_deltas::token.eq("t"),
            undo_alias_deltas::path.eq("p"),
            undo_alias_deltas::existed.eq(0_i64),
            undo_alias_deltas::canonical_target.eq(Option::<String>::None),
            undo_alias_deltas::resolved_kind.eq(Option::<i64>::None),
            undo_alias_deltas::resolved_hash.eq(Option::<ContentHash>::None),
            undo_alias_deltas::resolved_size.eq(Option::<i64>::None),
            undo_alias_deltas::modified.eq(Option::<i64>::None),
        ));
        assert_eq!(rendered(&alias), rendered(&alias_old));
    }
}
