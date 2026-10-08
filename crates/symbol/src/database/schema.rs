use std::fmt::Write as _;

#[cfg(test)]
use sea_query::Query;
use sea_query::{
    Alias, ColumnDef, Expr, ExprTrait, ForeignKey, ForeignKeyAction, ForeignKeyCreateStatement,
    Iden, Index, IndexCreateStatement, IntoIden, IntoTableRef, SchemaStatementBuilder,
    SqliteQueryBuilder, Table, TableCreateStatement,
};

pub const LATEST_SCHEMA_VERSION: i64 = 12;
pub const FILE_ENTRY_KIND: i64 = 0;
pub const ALLOCATED_ENTRY_KIND: i64 = 1;
pub const ALIAS_ENTRY_KIND: i64 = 2;

#[derive(Iden)]
enum Sites {
    Table,
    Id,
    Name,
    Created,
    Updated,
    PublicUrl,
    ContentRevision,
    TreeHash,
    CreatorKind,
    CreatorHash,
    ClaimHash,
    ManagementHash,
    ManagementStatus,
}

#[derive(Iden)]
enum Blobs {
    Table,
    Hash,
    Bytes,
    Size,
}

#[derive(Iden)]
enum Files {
    Table,
    SiteId,
    Path,
    Kind,
    Hash,
    Size,
    Modified,
}

#[derive(Iden)]
enum SiteEntries {
    Table,
    SiteId,
    Path,
    Kind,
}

#[derive(Iden)]
enum UndoFileDeltas {
    Table,
    Token,
    Path,
    Existed,
    Kind,
    Hash,
    Size,
    Modified,
}

#[derive(Iden)]
enum AllocatedEntries {
    Table,
    SiteId,
    Path,
    Kind,
    Hash,
    Size,
    NamingMode,
    Prefix,
    Suffix,
    Extension,
    MediaType,
    Modified,
}

#[derive(Iden)]
enum PendingAllocations {
    Table,
    Token,
    SiteId,
    Folder,
    Hash,
    Size,
    MediaType,
    RequestFingerprint,
    Created,
    Expires,
}

#[derive(Iden)]
enum UndoAllocatedDeltas {
    Table,
    Token,
    Path,
    Existed,
    Hash,
    Size,
    NamingMode,
    Prefix,
    Suffix,
    Extension,
    MediaType,
    Modified,
}

#[derive(Iden)]
enum Aliases {
    Table,
    SiteId,
    Path,
    Kind,
    CanonicalTarget,
    ResolvedKind,
    ResolvedHash,
    ResolvedSize,
    Modified,
}

#[derive(Iden)]
enum UndoAliasDeltas {
    Table,
    Token,
    Path,
    Existed,
    CanonicalTarget,
    ResolvedKind,
    ResolvedHash,
    ResolvedSize,
    Modified,
}

#[derive(Iden)]
enum SiteEvents {
    Table,
    Id,
    SiteId,
    Kind,
    Occurred,
    Files,
}

#[derive(Iden)]
enum Metadata {
    Table,
    Key,
    Value,
}

#[derive(Iden)]
enum UndoOperations {
    Table,
    Token,
    Kind,
    Description,
    Created,
    Expires,
    Consumed,
}

#[derive(Iden)]
enum UndoNames {
    Table,
    Token,
    Name,
}

#[derive(Iden)]
enum UndoSites {
    Table,
    Token,
    Name,
    Existed,
    PublicUrl,
    Created,
    Updated,
    ContentRevision,
    TreeHash,
}

#[derive(Iden)]
enum UndoFiles {
    Table,
    Token,
    Path,
    Hash,
    Size,
    Modified,
}

#[derive(Iden)]
enum ExpiryPolicies {
    Table,
    SiteId,
    Path,
    TargetKind,
    Mode,
    DurationSeconds,
    Deadline,
    MinAgeSeconds,
    MaxAgeSeconds,
    MaxSizeBytes,
    Power,
    Refreshed,
    OwnDeadline,
    SizeBytes,
}

#[derive(Iden)]
enum UndoExpiryPolicies {
    Table,
    Token,
    Path,
    TargetKind,
    Mode,
    DurationSeconds,
    Deadline,
    MinAgeSeconds,
    MaxAgeSeconds,
    MaxSizeBytes,
    Power,
    Refreshed,
    OwnDeadline,
    SizeBytes,
}

#[derive(Iden)]
enum IdempotencyRecords {
    Table,
    KeyHash,
    Fingerprint,
    OperationKind,
    ResultMetadata,
    Expires,
}

#[derive(Iden)]
enum ManagementTombstones {
    Table,
    Name,
    ManagementHash,
    Created,
}

#[derive(Iden)]
enum ManagementAudit {
    Table,
    Id,
    SiteName,
    Action,
    Occurred,
    SourceIp,
}

#[derive(Iden)]
enum ManagementIdempotency {
    Table,
    KeyHash,
    Fingerprint,
    Expires,
}

#[derive(Iden)]
enum PathAggregates {
    Table,
    SiteId,
    Path,
    LogicalBytes,
    FileCount,
}

/// How the tables that carry content hashes were shaped at a schema version.
///
/// Both flags only ever turned on: the hash columns became 32-byte blobs in v11
/// and the `modified` columns arrived in v12.
#[derive(Clone, Copy)]
struct Layout {
    /// Hashes are 32-byte blobs rather than lowercase hex text.
    binary_hashes: bool,
    /// The `modified` columns exist.
    modified: bool,
}

impl Layout {
    const CURRENT: Self = Self {
        binary_hashes: true,
        modified: true,
    };
    const V11: Self = Self {
        binary_hashes: true,
        modified: false,
    };
    const V10: Self = Self {
        binary_hashes: false,
        modified: false,
    };

    fn hash_column(self, iden: impl IntoIden) -> ColumnDef {
        if self.binary_hashes {
            blob(iden)
        } else {
            text(iden)
        }
    }

    /// Expression rewriting `column` from the other hash representation into
    /// this one.
    fn convert_hash(self, column: &str) -> String {
        if self.binary_hashes {
            format!("unhex(\"{column}\")")
        } else {
            format!("lower(hex(\"{column}\"))")
        }
    }

    /// [`Self::convert_hash`] for a column that may be `NULL`.
    fn convert_nullable_hash(self, column: &str) -> String {
        format!(
            "CASE WHEN \"{column}\" IS NULL THEN NULL ELSE {} END",
            self.convert_hash(column)
        )
    }
}

pub fn tables() -> Vec<TableCreateStatement> {
    let layout = Layout::CURRENT;
    vec![
        sites_table(),
        blobs_table(layout),
        site_events_table(),
        site_entries_table(),
        files_table(layout),
        metadata_table(),
        undo_operations_table(),
        undo_names_table(),
        undo_sites_table(),
        undo_files_table(layout),
        expiry_policies_table(),
        undo_expiry_policies_table(),
        idempotency_records_table(),
        management_tombstones_table(),
        management_audit_table(),
        management_idempotency_table(),
        path_aggregates_table(),
        undo_file_deltas_table(layout),
        allocated_entries_table(layout),
        pending_allocations_table(layout),
        undo_allocated_deltas_table(layout),
        aliases_table(layout),
        undo_alias_deltas_table(layout),
    ]
}

pub fn indexes() -> Vec<IndexCreateStatement> {
    vec![
        index("files_hash", Files::Table, [Files::Hash]),
        index(
            "files_site_prefix",
            Files::Table,
            [Files::SiteId, Files::Path],
        ),
        index(
            "files_site_hash",
            Files::Table,
            [Files::SiteId, Files::Hash],
        ),
        index(
            "undo_operations_retention",
            UndoOperations::Table,
            [
                UndoOperations::Consumed,
                UndoOperations::Expires,
                UndoOperations::Created,
            ],
        ),
        index(
            "undo_names_stack",
            UndoNames::Table,
            [UndoNames::Name, UndoNames::Token],
        ),
        index("undo_files_hash", UndoFiles::Table, [UndoFiles::Hash]),
        index(
            "expiry_policies_deadline",
            ExpiryPolicies::Table,
            [ExpiryPolicies::OwnDeadline],
        ),
        index(
            "expiry_policies_site_kind",
            ExpiryPolicies::Table,
            [
                ExpiryPolicies::SiteId,
                ExpiryPolicies::TargetKind,
                ExpiryPolicies::Path,
            ],
        ),
        index(
            "idempotency_records_expiry",
            IdempotencyRecords::Table,
            [IdempotencyRecords::Expires],
        ),
        index(
            "management_idempotency_expiry",
            ManagementIdempotency::Table,
            [ManagementIdempotency::Expires],
        ),
        index(
            "management_audit_site",
            ManagementAudit::Table,
            [ManagementAudit::SiteName, ManagementAudit::Occurred],
        ),
        index(
            "pending_allocations_expiry",
            PendingAllocations::Table,
            [PendingAllocations::Expires],
        ),
        index(
            "aliases_dependency",
            Aliases::Table,
            [Aliases::SiteId, Aliases::CanonicalTarget],
        ),
        index(
            "site_events_site",
            SiteEvents::Table,
            [SiteEvents::SiteId, SiteEvents::Occurred],
        ),
        index(
            "aliases_cache",
            Aliases::Table,
            [
                Aliases::SiteId,
                Aliases::ResolvedKind,
                Aliases::ResolvedHash,
                Aliases::ResolvedSize,
            ],
        ),
    ]
}

/// The indexes of [`indexes`] that existed at v6, in schema order.
const V6_INDEXES: [&str; 10] = [
    "files_hash",
    "files_site_prefix",
    "undo_operations_retention",
    "undo_names_stack",
    "undo_files_hash",
    "expiry_policies_deadline",
    "expiry_policies_site_kind",
    "idempotency_records_expiry",
    "management_idempotency_expiry",
    "management_audit_site",
];

/// The `files` indexes, which every rebuild of `files` has to recreate.
const FILES_INDEXES: [&str; 3] = ["files_hash", "files_site_prefix", "files_site_hash"];

const ALIASES_INDEXES: [&str; 2] = ["aliases_dependency", "aliases_cache"];

pub fn schema_sql() -> String {
    render_schema(&tables(), &indexes(), LATEST_SCHEMA_VERSION)
}

pub fn schema_v6_sql() -> String {
    let layout = Layout::V10;
    let tables = [
        sites_v6_table(),
        blobs_table(layout),
        files_v6_table(),
        metadata_table(),
        undo_operations_table(),
        undo_names_table(),
        undo_sites_v6_table(),
        undo_files_table(layout),
        expiry_policies_table(),
        undo_expiry_policies_table(),
        idempotency_records_table(),
        management_tombstones_table(),
        management_audit_table(),
        management_idempotency_table(),
        path_aggregates_table(),
    ];
    let indexes: Vec<_> = V6_INDEXES.iter().map(|name| named_index(name)).collect();
    render_schema(&tables, &indexes, 6)
}

fn render_schema(
    tables: &[TableCreateStatement],
    indexes: &[IndexCreateStatement],
    user_version: i64,
) -> String {
    let mut sql = String::from("-- AUTO-GENERATED FROM crates/symbol/src/database/schema.rs\n");
    sql.push_str("PRAGMA foreign_keys = ON;\n\n");
    for table in tables {
        sql.push_str(&to_sql(table));
        sql.push_str(";\n\n");
    }
    for index in indexes {
        sql.push_str(&to_sql(index));
        sql.push_str(";\n");
    }
    writeln!(sql, "\nPRAGMA user_version = {user_version};")
        .expect("writing to String cannot fail");
    sql
}

pub fn upgrade_v2_to_v6() -> Vec<String> {
    vec![
        add_column(
            ExpiryPolicies::Table,
            int_nn_zero(ExpiryPolicies::SizeBytes),
        ),
        to_sql(&undo_expiry_policies_table()),
        to_sql(&plain_index(
            "expiry_policies_site_kind",
            ExpiryPolicies::Table,
            [
                ExpiryPolicies::SiteId,
                ExpiryPolicies::TargetKind,
                ExpiryPolicies::Path,
            ],
        )),
        add_column(Sites::Table, int(Sites::CreatorKind)),
        add_column(Sites::Table, blob(Sites::CreatorHash)),
        add_column(Sites::Table, blob(Sites::ClaimHash)),
        add_column(Sites::Table, blob(Sites::ManagementHash)),
        add_column(Sites::Table, int_nn_zero(Sites::ManagementStatus)),
        to_sql(&management_tombstones_table()),
        to_sql(&management_audit_table()),
        to_sql(&management_idempotency_table()),
        to_sql(&plain_index(
            "management_idempotency_expiry",
            ManagementIdempotency::Table,
            [ManagementIdempotency::Expires],
        )),
        to_sql(&plain_index(
            "management_audit_site",
            ManagementAudit::Table,
            [ManagementAudit::SiteName, ManagementAudit::Occurred],
        )),
        to_sql(&path_aggregates_table()),
    ]
}

pub fn upgrade_v6_to_v7_before_copy() -> Vec<String> {
    vec![to_sql(&site_entries_table())]
}

pub fn upgrade_v6_to_v7_after_backfill() -> Vec<String> {
    vec![
        rename(Files::Table, Alias::new("files_v6")),
        to_sql(&files_table(Layout::V10)),
    ]
}

pub fn upgrade_v6_to_v7_after_file_copy() -> Vec<String> {
    vec![
        drop_table(Alias::new("files_v6")),
        named_index_sql("files_hash"),
        named_index_sql("files_site_prefix"),
        to_sql(&undo_file_deltas_table(Layout::V10)),
    ]
}

pub fn upgrade_v7_to_v8() -> Vec<String> {
    let layout = Layout::V10;
    vec![
        to_sql(&allocated_entries_table(layout)),
        to_sql(&pending_allocations_table(layout)),
        to_sql(&undo_allocated_deltas_table(layout)),
        named_index_sql("files_site_hash"),
        named_index_sql("pending_allocations_expiry"),
    ]
}

pub fn upgrade_v9_to_v10() -> Vec<String> {
    vec![
        add_column(Sites::Table, int(Sites::Created)),
        add_column(UndoSites::Table, int(UndoSites::Created)),
        to_sql(&site_events_table()),
        named_index_sql("site_events_site"),
    ]
}

/// Converts every content hash from lowercase hex text to a 32-byte blob.
pub fn upgrade_v10_to_v11() -> Vec<String> {
    let target = Layout::V11;
    let mut statements = vec!["PRAGMA foreign_keys=OFF".to_string()];
    statements.extend(recreate_blobs(target));
    statements.extend(convert_hash_tables(target));
    statements.extend(convert_tree_hash("sites", target));
    statements.extend(convert_tree_hash("undo_sites", target));
    statements.push(drop_table(Alias::new("blobs_v10")));
    statements.push("PRAGMA foreign_keys=ON".to_string());
    statements
}

/// The tables that record when an entry's content last changed, and the
/// undo tables that snapshot them.
const MODIFIED_TABLES: [&str; 3] = ["files", "allocated_entries", "aliases"];
#[cfg(test)]
const UNDO_MODIFIED_TABLES: [&str; 4] = [
    "undo_files",
    "undo_file_deltas",
    "undo_allocated_deltas",
    "undo_alias_deltas",
];

/// Adds `modified`: when each entry's content last changed, in Unix
/// milliseconds.
///
/// Entries that predate the column are backfilled with their site's `updated`,
/// the latest moment their content can have changed. Undo snapshots taken
/// before the column existed leave it `NULL`, and restoring one falls back to
/// the snapshot's own `updated` for the same reason.
pub fn upgrade_v11_to_v12() -> Vec<String> {
    let mut statements = vec![
        add_column(Files::Table, modified_column(Files::Modified)),
        add_column(
            AllocatedEntries::Table,
            modified_column(AllocatedEntries::Modified),
        ),
        add_column(Aliases::Table, modified_column(Aliases::Modified)),
        add_column(UndoFiles::Table, undo_modified_column(UndoFiles::Modified)),
        add_column(
            UndoFileDeltas::Table,
            undo_modified_column(UndoFileDeltas::Modified),
        ),
        add_column(
            UndoAllocatedDeltas::Table,
            undo_modified_column(UndoAllocatedDeltas::Modified),
        ),
        add_column(
            UndoAliasDeltas::Table,
            undo_modified_column(UndoAliasDeltas::Modified),
        ),
    ];
    for table in MODIFIED_TABLES {
        statements.push(format!(
            "UPDATE \"{table}\" SET \"modified\" = \
             (SELECT \"updated\" FROM \"sites\" WHERE \"sites\".\"id\" = \"{table}\".\"site_id\")"
        ));
    }
    statements
}

#[cfg(test)]
pub fn downgrade_v12_to_v11() -> Vec<String> {
    MODIFIED_TABLES
        .iter()
        .chain(UNDO_MODIFIED_TABLES.iter())
        .map(|table| format!("ALTER TABLE \"{table}\" DROP COLUMN \"modified\""))
        .collect()
}

fn modified_column(iden: impl IntoIden) -> ColumnDef {
    int_nn_zero(iden)
}

fn undo_modified_column(iden: impl IntoIden) -> ColumnDef {
    int(iden)
}

/// Converts every content hash from a 32-byte blob back to lowercase hex text.
#[cfg(test)]
pub fn downgrade_v11_to_v10() -> Vec<String> {
    let target = Layout::V10;
    let mut statements = convert_tree_hash("sites", target);
    statements.extend(convert_tree_hash("undo_sites", target));
    statements.extend(recreate_blobs(target));
    statements.push(drop_table(Alias::new("blobs_v10")));
    statements.extend(convert_hash_tables(target));
    statements
}

/// Rebuilds `blobs` with the hash representation of `target`.
///
/// The old table is left behind: the tables that reference it are rebuilt by
/// [`convert_hash_tables`], so the caller decides when it can be dropped.
fn recreate_blobs(target: Layout) -> Vec<String> {
    let hash = target.convert_hash("hash");
    recreate_table(
        "blobs",
        "blobs_v10",
        &blobs_table(target),
        format!(
            "INSERT INTO \"blobs\" (\"hash\", \"bytes\", \"size\")
         SELECT {hash}, \"bytes\", \"size\" FROM \"blobs_v10\""
        ),
    )
}

/// Rebuilds every table that holds a content hash, other than `blobs` itself,
/// with the hash representation of `target`.
///
/// Shared by [`upgrade_v10_to_v11`] and `downgrade_v11_to_v10`: the two only
/// differ in which way the hashes are rewritten.
fn convert_hash_tables(target: Layout) -> Vec<String> {
    let mut statements = convert_blob_referencing_tables(target);
    statements.extend(convert_nullable_hash_tables(target));
    statements
}

/// The tables whose `NOT NULL` hash references `blobs`.
fn convert_blob_referencing_tables(target: Layout) -> Vec<String> {
    let hash = target.convert_hash("hash");
    let mut statements = Vec::new();
    statements.extend(rebuild_table(
        "files",
        "files_v10",
        &files_table(target),
        format!(
            "INSERT INTO \"files\" (\"site_id\", \"path\", \"kind\", \"hash\", \"size\")
         SELECT \"site_id\", \"path\", \"kind\", {hash}, \"size\" FROM \"files_v10\""
        ),
    ));
    statements.extend(named_index_sqls(&FILES_INDEXES));
    statements.extend(rebuild_table(
        "undo_files",
        "undo_files_v10",
        &undo_files_table(target),
        format!(
            "INSERT INTO \"undo_files\" (\"token\", \"path\", \"hash\", \"size\")
         SELECT \"token\", \"path\", {hash}, \"size\" FROM \"undo_files_v10\""
        ),
    ));
    statements.push(named_index_sql("undo_files_hash"));
    statements.extend(rebuild_table(
        "allocated_entries",
        "allocated_entries_v10",
        &allocated_entries_table(target),
        format!(
            "INSERT INTO \"allocated_entries\"
            (\"site_id\", \"path\", \"kind\", \"hash\", \"size\", \"naming_mode\", \"prefix\",
             \"suffix\", \"extension\", \"media_type\")
         SELECT \"site_id\", \"path\", \"kind\", {hash}, \"size\", \"naming_mode\",
                \"prefix\", \"suffix\", \"extension\", \"media_type\"
         FROM \"allocated_entries_v10\""
        ),
    ));
    statements.extend(rebuild_table(
        "pending_allocations",
        "pending_allocations_v10",
        &pending_allocations_table(target),
        format!(
            "INSERT INTO \"pending_allocations\"
            (\"token\", \"site_id\", \"folder\", \"hash\", \"size\", \"media_type\",
             \"request_fingerprint\", \"created\", \"expires\")
         SELECT \"token\", \"site_id\", \"folder\", {hash}, \"size\", \"media_type\",
                \"request_fingerprint\", \"created\", \"expires\"
         FROM \"pending_allocations_v10\""
        ),
    ));
    statements.push(named_index_sql("pending_allocations_expiry"));
    statements
}

/// The tables whose hash is nullable and not a foreign key.
fn convert_nullable_hash_tables(target: Layout) -> Vec<String> {
    let nullable_hash = target.convert_nullable_hash("hash");
    let resolved_hash = target.convert_nullable_hash("resolved_hash");
    let mut statements = Vec::new();
    statements.extend(rebuild_table(
        "undo_file_deltas",
        "undo_file_deltas_v10",
        &undo_file_deltas_table(target),
        format!(
            "INSERT INTO \"undo_file_deltas\"
            (\"token\", \"path\", \"existed\", \"kind\", \"hash\", \"size\")
         SELECT \"token\", \"path\", \"existed\", \"kind\",
                {nullable_hash},
                \"size\"
         FROM \"undo_file_deltas_v10\""
        ),
    ));
    statements.extend(rebuild_table(
        "undo_allocated_deltas",
        "undo_allocated_deltas_v10",
        &undo_allocated_deltas_table(target),
        format!(
            "INSERT INTO \"undo_allocated_deltas\"
            (\"token\", \"path\", \"existed\", \"hash\", \"size\", \"naming_mode\", \"prefix\",
             \"suffix\", \"extension\", \"media_type\")
         SELECT \"token\", \"path\", \"existed\",
                {nullable_hash},
                \"size\", \"naming_mode\", \"prefix\", \"suffix\", \"extension\", \"media_type\"
         FROM \"undo_allocated_deltas_v10\""
        ),
    ));
    statements.extend(rebuild_table(
        "aliases",
        "aliases_v10",
        &aliases_table(target),
        format!(
            "INSERT INTO \"aliases\"
            (\"site_id\", \"path\", \"kind\", \"canonical_target\", \"resolved_kind\",
             \"resolved_hash\", \"resolved_size\")
         SELECT \"site_id\", \"path\", \"kind\", \"canonical_target\", \"resolved_kind\",
                {resolved_hash},
                \"resolved_size\"
         FROM \"aliases_v10\""
        ),
    ));
    statements.extend(named_index_sqls(&ALIASES_INDEXES));
    statements.extend(rebuild_table(
        "undo_alias_deltas",
        "undo_alias_deltas_v10",
        &undo_alias_deltas_table(target),
        format!(
            "INSERT INTO \"undo_alias_deltas\"
            (\"token\", \"path\", \"existed\", \"canonical_target\", \"resolved_kind\",
             \"resolved_hash\", \"resolved_size\")
         SELECT \"token\", \"path\", \"existed\", \"canonical_target\", \"resolved_kind\",
                {resolved_hash},
                \"resolved_size\"
         FROM \"undo_alias_deltas_v10\""
        ),
    ));
    statements
}

/// Rewrites `tree_hash` of `table` into the representation of `target`.
///
/// Goes through a temporary column rather than rebuilding the table, which
/// would rewrite the `REFERENCES "sites"` clause of every child table.
fn convert_tree_hash(table: &str, target: Layout) -> Vec<String> {
    let (column, column_type, value) = if target.binary_hashes {
        (
            "tree_hash_bin",
            "BLOB",
            "CASE
            WHEN \"tree_hash\" = '' THEN zeroblob(32)
            WHEN \"tree_hash\" LIKE 'blake3:%' THEN unhex(substr(\"tree_hash\", 8))
            ELSE unhex(\"tree_hash\")
        END",
        )
    } else {
        (
            "tree_hash_text",
            "TEXT",
            "CASE
            WHEN \"tree_hash\" IS NULL OR \"tree_hash\" = zeroblob(32) THEN ''
            ELSE lower(hex(\"tree_hash\"))
        END",
        )
    };
    vec![
        format!("ALTER TABLE \"{table}\" ADD COLUMN \"{column}\" {column_type}"),
        format!("UPDATE \"{table}\" SET \"{column}\" = {value}"),
        format!("ALTER TABLE \"{table}\" DROP COLUMN \"tree_hash\""),
        format!("ALTER TABLE \"{table}\" RENAME COLUMN \"{column}\" TO \"tree_hash\""),
    ]
}

pub fn upgrade_v8_to_v9() -> Vec<String> {
    let layout = Layout::V10;
    let mut statements = vec![
        rename(Files::Table, Alias::new("files_v8")),
        rename(AllocatedEntries::Table, Alias::new("allocated_entries_v8")),
        to_sql(&files_table(layout)),
        to_sql(&allocated_entries_table(layout)),
        "INSERT INTO \"files\" (\"site_id\", \"path\", \"kind\", \"hash\", \"size\")
         SELECT \"site_id\", \"path\", \"kind\", \"hash\", \"size\" FROM \"files_v8\""
            .to_string(),
        "INSERT INTO \"allocated_entries\"
            (\"site_id\", \"path\", \"kind\", \"hash\", \"size\", \"naming_mode\", \"prefix\",
             \"suffix\", \"extension\", \"media_type\")
         SELECT \"site_id\", \"path\", \"kind\", \"hash\", \"size\", \"naming_mode\", \"prefix\",
                \"suffix\", \"extension\", \"media_type\"
         FROM \"allocated_entries_v8\""
            .to_string(),
        drop_table(Alias::new("files_v8")),
        drop_table(Alias::new("allocated_entries_v8")),
    ];
    statements.extend(named_index_sqls(&FILES_INDEXES));
    statements.push(to_sql(&aliases_table(layout)));
    statements.push(to_sql(&undo_alias_deltas_table(layout)));
    statements.extend(named_index_sqls(&ALIASES_INDEXES));
    statements
}

#[cfg(test)]
pub fn downgrade_v9_to_v6_before_copy() -> Vec<String> {
    vec![
        drop_table(SiteEvents::Table),
        "ALTER TABLE \"sites\" DROP COLUMN \"created\"".to_string(),
        "ALTER TABLE \"undo_sites\" DROP COLUMN \"created\"".to_string(),
        drop_table(UndoAliasDeltas::Table),
        drop_table(Aliases::Table),
        drop_table(UndoAllocatedDeltas::Table),
        drop_table(PendingAllocations::Table),
        drop_table(AllocatedEntries::Table),
        drop_table(UndoFileDeltas::Table),
        rename(Files::Table, Alias::new("files_v9")),
        to_sql(&files_v6_table()),
    ]
}

#[cfg(test)]
pub fn downgrade_v9_to_v6_after_copy() -> Vec<String> {
    let mut statements = vec![
        drop_table(Alias::new("files_v9")),
        drop_table(SiteEntries::Table),
    ];
    statements.extend(named_index_sqls(&["files_hash", "files_site_prefix"]));
    statements
}

#[cfg(test)]
pub fn insert_v6_file(site_id: i64, path: &str, hash: &str, size: i64) -> String {
    Query::insert()
        .into_table(Files::Table)
        .columns([Files::SiteId, Files::Path, Files::Hash, Files::Size])
        .values_panic([site_id.into(), path.into(), hash.into(), size.into()])
        .to_owned()
        .to_string(SqliteQueryBuilder)
}

#[cfg(test)]
pub fn downgrade_v6_to_v2() -> Vec<String> {
    vec![
        drop_table(PathAggregates::Table),
        drop_index(
            "management_idempotency_expiry",
            ManagementIdempotency::Table,
        ),
        drop_index("management_audit_site", ManagementAudit::Table),
        drop_table(ManagementIdempotency::Table),
        drop_table(ManagementAudit::Table),
        drop_table(ManagementTombstones::Table),
        drop_column(Sites::Table, Sites::CreatorKind),
        drop_column(Sites::Table, Sites::CreatorHash),
        drop_column(Sites::Table, Sites::ClaimHash),
        drop_column(Sites::Table, Sites::ManagementHash),
        drop_column(Sites::Table, Sites::ManagementStatus),
        drop_index("expiry_policies_site_kind", ExpiryPolicies::Table),
        drop_table(UndoExpiryPolicies::Table),
        drop_column(ExpiryPolicies::Table, ExpiryPolicies::SizeBytes),
    ]
}

/// Rebuild the v6 tables that `ALTER TABLE` cannot bring back into canonical shape.
///
/// `upgrade_v2_to_v6` only adds columns, which is enough for a database that
/// was created as v2. It is not enough for one that reached v2 by downgrade:
/// `downgrade_v11_to_v10` rebuilds `sites.tree_hash` through a temporary
/// column, so it comes back appended to the end of the table, nullable, and
/// without its default. `validate_v6_catalog` compares column position, type,
/// nullability and defaults, so it rejects that table until it is rewritten --
/// hence the `COALESCE("tree_hash", '')` below. The same applies to the child
/// tables that have to be copied out and back around the `sites` rewrite.
///
/// Removing this step fails `v2_upgrade_reaches_v9_and_preserves_legacy_files`
/// with `CatalogDifference { table: "sites", dimension: Columns }`. For a
/// database that really was created as v2 the rebuild is redundant but
/// harmless.
pub fn normalize_v6_schema() -> Vec<String> {
    let layout = Layout::V10;
    let mut statements = vec![
        "CREATE TABLE \"files_repair_source\" AS
         SELECT \"site_id\", \"path\", \"hash\", \"size\" FROM \"files\""
            .to_string(),
        "CREATE TABLE \"undo_files_repair_source\" AS
         SELECT \"token\", \"path\", \"hash\", \"size\" FROM \"undo_files\""
            .to_string(),
    ];
    statements.extend(rebuild_table(
        "sites",
        "sites_repair",
        &sites_v6_table(),
        "INSERT INTO \"sites\"
            (\"id\", \"name\", \"updated\", \"public_url\", \"content_revision\", \"tree_hash\",
             \"creator_kind\", \"creator_hash\", \"claim_hash\", \"management_hash\",
             \"management_status\")
         SELECT \"id\", \"name\", \"updated\", \"public_url\", \"content_revision\",
                COALESCE(\"tree_hash\", ''),
                \"creator_kind\", \"creator_hash\", \"claim_hash\", \"management_hash\",
                \"management_status\"
         FROM \"sites_repair\"",
    ));
    statements.extend(rebuild_table(
        "expiry_policies",
        "expiry_policies_repair",
        &expiry_policies_table(),
        "INSERT INTO \"expiry_policies\"
            (\"site_id\", \"path\", \"target_kind\", \"mode\", \"duration_seconds\", \"deadline\",
             \"min_age_seconds\", \"max_age_seconds\", \"max_size_bytes\", \"power\", \"refreshed\",
             \"own_deadline\")
         SELECT \"site_id\", \"path\", \"target_kind\", \"mode\", \"duration_seconds\", \"deadline\",
                \"min_age_seconds\", \"max_age_seconds\", \"max_size_bytes\", \"power\", \"refreshed\",
                \"own_deadline\"
         FROM \"expiry_policies_repair\"",
    ));
    statements.push(to_sql(&plain_index(
        "expiry_policies_site_kind",
        ExpiryPolicies::Table,
        [
            ExpiryPolicies::SiteId,
            ExpiryPolicies::TargetKind,
            ExpiryPolicies::Path,
        ],
    )));
    statements.push(to_sql(&plain_index(
        "expiry_policies_deadline",
        ExpiryPolicies::Table,
        [ExpiryPolicies::OwnDeadline],
    )));
    statements.extend(rebuild_table(
        "blobs",
        "blobs_repair",
        &blobs_table(layout),
        "INSERT INTO \"blobs\" (\"hash\", \"bytes\", \"size\")
         SELECT \"hash\", \"bytes\", \"size\" FROM \"blobs_repair\"",
    ));
    statements.extend(rebuild_table(
        "files",
        "files_repair",
        &files_v6_table(),
        "INSERT INTO \"files\" (\"site_id\", \"path\", \"hash\", \"size\")
         SELECT \"site_id\", \"path\", \"hash\", \"size\" FROM \"files_repair_source\"",
    ));
    statements.push(drop_table(Alias::new("files_repair_source")));
    statements.extend(named_index_sqls(&["files_hash", "files_site_prefix"]));
    statements.extend(rebuild_table(
        "undo_files",
        "undo_files_repair",
        &undo_files_table(layout),
        "INSERT INTO \"undo_files\" (\"token\", \"path\", \"hash\", \"size\")
         SELECT \"token\", \"path\", \"hash\", \"size\" FROM \"undo_files_repair_source\"",
    ));
    statements.push(drop_table(Alias::new("undo_files_repair_source")));
    statements.push(named_index_sql("undo_files_hash"));
    statements.extend(rebuild_table(
        "undo_sites",
        "undo_sites_repair",
        &undo_sites_v6_table(),
        "INSERT INTO \"undo_sites\"
            (\"token\", \"name\", \"existed\", \"public_url\", \"updated\", \"content_revision\",
             \"tree_hash\")
         SELECT \"token\", \"name\", \"existed\", \"public_url\", \"updated\", \"content_revision\",
                COALESCE(\"tree_hash\", '')
         FROM \"undo_sites_repair\"",
    ));
    statements.extend(rebuild_table(
        "path_aggregates",
        "path_aggregates_repair",
        &path_aggregates_table(),
        "INSERT INTO \"path_aggregates\"
            (\"site_id\", \"path\", \"logical_bytes\", \"file_count\")
         SELECT \"site_id\", \"path\", \"logical_bytes\", \"file_count\"
         FROM \"path_aggregates_repair\"",
    ));
    statements
}

/// One table rebuilt by [`normalize_v11_schema`].
struct V11RepairTable {
    /// Live table name. Its snapshot is this name plus `_v11_source`.
    name: &'static str,
    /// Columns carried across, in canonical order.
    columns: &'static [&'static str],
    /// Restore expressions, when the copy is not a plain column-for-column one.
    ///
    /// Positional: entry `i` overrides `columns[i]`.
    restore: &'static [(usize, &'static str)],
}

/// Tables rebuilt by [`normalize_v11_schema`], parents before children.
///
/// Everything except `undo_sites` is `sites` or one of its descendants, so this
/// order is a safe restore order and its reverse is a safe drop order.
const V11_REPAIR_TABLES: &[V11RepairTable] = &[
    V11RepairTable {
        name: "sites",
        columns: &[
            "id",
            "name",
            "created",
            "updated",
            "public_url",
            "content_revision",
            "tree_hash",
            "creator_kind",
            "creator_hash",
            "claim_hash",
            "management_hash",
            "management_status",
        ],
        restore: &[(6, "COALESCE(tree_hash, zeroblob(32))")],
    },
    V11RepairTable {
        name: "site_entries",
        columns: &["site_id", "path", "kind"],
        restore: &[],
    },
    V11RepairTable {
        name: "site_events",
        columns: &["id", "site_id", "kind", "occurred", "files"],
        restore: &[],
    },
    V11RepairTable {
        name: "expiry_policies",
        columns: &[
            "site_id",
            "path",
            "target_kind",
            "mode",
            "duration_seconds",
            "deadline",
            "min_age_seconds",
            "max_age_seconds",
            "max_size_bytes",
            "power",
            "refreshed",
            "own_deadline",
            "size_bytes",
        ],
        restore: &[],
    },
    V11RepairTable {
        name: "path_aggregates",
        columns: &["site_id", "path", "logical_bytes", "file_count"],
        restore: &[],
    },
    V11RepairTable {
        name: "pending_allocations",
        columns: &[
            "token",
            "site_id",
            "folder",
            "hash",
            "size",
            "media_type",
            "request_fingerprint",
            "created",
            "expires",
        ],
        restore: &[],
    },
    V11RepairTable {
        name: "files",
        columns: &["site_id", "path", "kind", "hash", "size", "modified"],
        restore: &[],
    },
    V11RepairTable {
        name: "allocated_entries",
        columns: &[
            "site_id",
            "path",
            "kind",
            "hash",
            "size",
            "naming_mode",
            "prefix",
            "suffix",
            "extension",
            "media_type",
            "modified",
        ],
        restore: &[],
    },
    V11RepairTable {
        name: "aliases",
        columns: &[
            "site_id",
            "path",
            "kind",
            "canonical_target",
            "resolved_kind",
            "resolved_hash",
            "resolved_size",
            "modified",
        ],
        restore: &[],
    },
    V11RepairTable {
        name: "undo_sites",
        columns: &[
            "token",
            "name",
            "existed",
            "public_url",
            "created",
            "updated",
            "content_revision",
            "tree_hash",
        ],
        restore: &[(7, "COALESCE(tree_hash, zeroblob(32))")],
    },
];

impl V11RepairTable {
    fn snapshot(&self) -> String {
        format!("{}_v11_source", self.name)
    }

    fn column_list(&self) -> String {
        self.columns
            .iter()
            .map(|column| quote_ident(column))
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn restore_list(&self) -> String {
        self.columns
            .iter()
            .enumerate()
            .map(|(position, column)| {
                self.restore
                    .iter()
                    .find(|(index, _)| *index == position)
                    .map_or_else(
                        || quote_ident(column),
                        |(_, expression)| (*expression).to_string(),
                    )
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn quote_ident(name: &str) -> String {
    debug_assert!(
        !name.contains('"'),
        "schema identifiers are fixed and never quoted"
    );
    let mut quoted = String::with_capacity(name.len() + 2);
    quoted.push('"');
    quoted.push_str(name);
    quoted.push('"');
    quoted
}

/// Rebuild the v11 tables that `ALTER TABLE` cannot bring into canonical shape.
///
/// `upgrade_v10_to_v11` converts `sites.tree_hash` and `undo_sites.tree_hash`
/// from hex text to a 32-byte blob through a temporary column, because renaming
/// `sites` would rewrite the `REFERENCES "sites"` clause of all five tables
/// that point at it. `PRAGMA foreign_keys=OFF` cannot prevent that: the pragma
/// is silently ignored inside a transaction, and every migration arm runs in
/// one.
///
/// The cost of the temporary-column route is that the rebuilt column comes back
/// appended to the end of the table, nullable, and without its
/// `DEFAULT x'00...'`, so an upgraded database does not match a fresh one. This
/// repair closes that gap by dropping and recreating the affected tables rather
/// than renaming them -- `DROP TABLE` does not rewrite anyone's `REFERENCES`
/// clause. Rows are parked in unconstrained snapshot tables first, because
/// dropping `sites` would otherwise cascade into its descendants.
///
/// Recreation replays the whole canonical table and index set. Every statement
/// is `IF NOT EXISTS`, so the tables that were not dropped are untouched and the
/// list cannot fall out of step with [`tables`] and [`indexes`].
///
/// This is the v11 counterpart of [`normalize_v6_schema`] and is likewise kept
/// out of the hashed migration program: it repairs a schema rather than
/// defining one, and folding it in would invalidate the recorded program hash
/// of every database already at v11.
pub fn normalize_v11_schema() -> Vec<String> {
    let mut statements = Vec::new();
    for table in V11_REPAIR_TABLES {
        statements.push(format!(
            "CREATE TABLE {} AS SELECT {} FROM {}",
            quote_ident(&table.snapshot()),
            table.column_list(),
            quote_ident(table.name),
        ));
    }
    for table in V11_REPAIR_TABLES.iter().rev() {
        statements.push(drop_table(Alias::new(table.name)));
    }
    statements.extend(tables().iter().map(to_sql));
    statements.extend(indexes().iter().map(to_sql));
    for table in V11_REPAIR_TABLES {
        statements.push(format!(
            "INSERT INTO {} ({}) SELECT {} FROM {}",
            quote_ident(table.name),
            table.column_list(),
            table.restore_list(),
            quote_ident(&table.snapshot()),
        ));
    }
    for table in V11_REPAIR_TABLES {
        statements.push(drop_table(Alias::new(table.snapshot())));
    }
    statements
}

fn sites_table() -> TableCreateStatement {
    create_table(Sites::Table)
        .col(int(Sites::Id).primary_key())
        .col(text_nn(Sites::Name).unique_key())
        .col(int(Sites::Created))
        .col(int_nn(Sites::Updated))
        .col(text_nn(Sites::PublicUrl).default(""))
        .col(int_nn_zero(Sites::ContentRevision))
        .col(blob_nn(Sites::TreeHash).default([0_u8; 32].to_vec()))
        .col(int(Sites::CreatorKind))
        .col(blob(Sites::CreatorHash))
        .col(blob(Sites::ClaimHash))
        .col(blob(Sites::ManagementHash))
        .col(int_nn_zero(Sites::ManagementStatus))
        .to_owned()
}

fn site_events_table() -> TableCreateStatement {
    create_table(SiteEvents::Table)
        .col(int(SiteEvents::Id).primary_key())
        .col(int_nn(SiteEvents::SiteId))
        .col(int_nn(SiteEvents::Kind))
        .col(int_nn(SiteEvents::Occurred))
        .col(int_nn_zero(SiteEvents::Files))
        .foreign_key(&mut fk_cascade(
            SiteEvents::Table,
            SiteEvents::SiteId,
            Sites::Table,
            Sites::Id,
        ))
        .to_owned()
}

fn blobs_table(layout: Layout) -> TableCreateStatement {
    create_table(Blobs::Table)
        .col(layout.hash_column(Blobs::Hash).primary_key())
        // Retired. Payloads moved to the on-disk blob tree in the
        // `external_blobs_v1` migration and every writer now stores an empty
        // vector here. The column is kept so that a database which has not run
        // that migration yet still matches this schema; dropping it would mean
        // another table rebuild for no gain.
        .col(blob_nn(Blobs::Bytes).default(Vec::<u8>::new()))
        .col(int_nn(Blobs::Size))
        .to_owned()
}

fn files_table(layout: Layout) -> TableCreateStatement {
    let mut table = create_table(Files::Table);
    table
        .col(int_nn(Files::SiteId))
        .col(text_nn(Files::Path))
        .col(
            int_nn(Files::Kind)
                .default(FILE_ENTRY_KIND)
                .check(Expr::col(Files::Kind).eq(FILE_ENTRY_KIND)),
        )
        .col(layout.hash_column(Files::Hash).not_null())
        .col(int_nn(Files::Size));
    if layout.modified {
        table.col(modified_column(Files::Modified));
    }
    table
        .primary_key(&mut primary_key([Files::SiteId, Files::Path]))
        .foreign_key(&mut site_entry_fk(
            Files::Table,
            Files::SiteId,
            Files::Path,
            Files::Kind,
        ))
        .foreign_key(&mut fk(
            Files::Table,
            Files::Hash,
            Blobs::Table,
            Blobs::Hash,
        ))
        .to_owned()
}

fn sites_v6_table() -> TableCreateStatement {
    create_table(Sites::Table)
        .col(int(Sites::Id).primary_key())
        .col(text_nn(Sites::Name).unique_key())
        .col(int_nn(Sites::Updated))
        .col(text_nn(Sites::PublicUrl).default(""))
        .col(int_nn_zero(Sites::ContentRevision))
        .col(text_nn(Sites::TreeHash).default(""))
        .col(int(Sites::CreatorKind))
        .col(blob(Sites::CreatorHash))
        .col(blob(Sites::ClaimHash))
        .col(blob(Sites::ManagementHash))
        .col(int_nn_zero(Sites::ManagementStatus))
        .to_owned()
}

fn undo_sites_v6_table() -> TableCreateStatement {
    create_table(UndoSites::Table)
        .col(text(UndoSites::Token).primary_key())
        .col(text_nn(UndoSites::Name))
        .col(int_nn(UndoSites::Existed))
        .col(text_nn(UndoSites::PublicUrl))
        .col(int_nn(UndoSites::Updated))
        .col(int_nn(UndoSites::ContentRevision))
        .col(text_nn(UndoSites::TreeHash))
        .foreign_key(&mut fk_cascade(
            UndoSites::Table,
            UndoSites::Token,
            UndoOperations::Table,
            UndoOperations::Token,
        ))
        .to_owned()
}

fn files_v6_table() -> TableCreateStatement {
    create_table(Files::Table)
        .col(int_nn(Files::SiteId))
        .col(text_nn(Files::Path))
        .col(text_nn(Files::Hash))
        .col(int_nn(Files::Size))
        .primary_key(&mut primary_key([Files::SiteId, Files::Path]))
        .foreign_key(&mut fk_cascade(
            Files::Table,
            Files::SiteId,
            Sites::Table,
            Sites::Id,
        ))
        .foreign_key(&mut fk(
            Files::Table,
            Files::Hash,
            Blobs::Table,
            Blobs::Hash,
        ))
        .to_owned()
}

fn site_entries_table() -> TableCreateStatement {
    create_table(SiteEntries::Table)
        .col(int_nn(SiteEntries::SiteId))
        .col(text_nn(SiteEntries::Path))
        .col(int_nn(SiteEntries::Kind))
        .primary_key(&mut primary_key([SiteEntries::SiteId, SiteEntries::Path]))
        .index(
            Index::create()
                .unique()
                .col(SiteEntries::SiteId)
                .col(SiteEntries::Path)
                .col(SiteEntries::Kind),
        )
        .foreign_key(&mut fk_cascade(
            SiteEntries::Table,
            SiteEntries::SiteId,
            Sites::Table,
            Sites::Id,
        ))
        .to_owned()
}

fn metadata_table() -> TableCreateStatement {
    create_table(Metadata::Table)
        .col(text(Metadata::Key).primary_key())
        .col(text_nn(Metadata::Value))
        .to_owned()
}

fn undo_operations_table() -> TableCreateStatement {
    create_table(UndoOperations::Table)
        .col(text(UndoOperations::Token).primary_key())
        .col(int_nn(UndoOperations::Kind))
        .col(text_nn(UndoOperations::Description))
        .col(int_nn(UndoOperations::Created))
        .col(int_nn(UndoOperations::Expires))
        .col(int_nn_zero(UndoOperations::Consumed))
        .to_owned()
}

fn undo_names_table() -> TableCreateStatement {
    create_table(UndoNames::Table)
        .col(text_nn(UndoNames::Token))
        .col(text_nn(UndoNames::Name))
        .primary_key(&mut primary_key([UndoNames::Token, UndoNames::Name]))
        .foreign_key(&mut fk_cascade(
            UndoNames::Table,
            UndoNames::Token,
            UndoOperations::Table,
            UndoOperations::Token,
        ))
        .to_owned()
}

fn undo_sites_table() -> TableCreateStatement {
    create_table(UndoSites::Table)
        .col(text(UndoSites::Token).primary_key())
        .col(text_nn(UndoSites::Name))
        .col(int_nn(UndoSites::Existed))
        .col(text_nn(UndoSites::PublicUrl))
        .col(int(UndoSites::Created))
        .col(int_nn(UndoSites::Updated))
        .col(int_nn(UndoSites::ContentRevision))
        .col(blob_nn(UndoSites::TreeHash))
        .foreign_key(&mut fk_cascade(
            UndoSites::Table,
            UndoSites::Token,
            UndoOperations::Table,
            UndoOperations::Token,
        ))
        .to_owned()
}

fn undo_files_table(layout: Layout) -> TableCreateStatement {
    let mut table = create_table(UndoFiles::Table);
    table
        .col(text_nn(UndoFiles::Token))
        .col(text_nn(UndoFiles::Path))
        .col(layout.hash_column(UndoFiles::Hash).not_null())
        .col(int_nn(UndoFiles::Size));
    if layout.modified {
        table.col(undo_modified_column(UndoFiles::Modified));
    }
    table
        .primary_key(&mut primary_key([UndoFiles::Token, UndoFiles::Path]))
        .foreign_key(&mut fk_cascade(
            UndoFiles::Table,
            UndoFiles::Token,
            UndoOperations::Table,
            UndoOperations::Token,
        ))
        .foreign_key(&mut fk(
            UndoFiles::Table,
            UndoFiles::Hash,
            Blobs::Table,
            Blobs::Hash,
        ))
        .to_owned()
}

fn expiry_policies_table() -> TableCreateStatement {
    let mut table = create_table(ExpiryPolicies::Table);
    table
        .col(int_nn(ExpiryPolicies::SiteId))
        .col(text_nn(ExpiryPolicies::Path))
        .col(int_nn(ExpiryPolicies::TargetKind))
        .col(int_nn(ExpiryPolicies::Mode));
    add_expiry_columns(&mut table);
    table
        .primary_key(&mut primary_key([
            ExpiryPolicies::SiteId,
            ExpiryPolicies::Path,
        ]))
        .foreign_key(&mut fk_cascade(
            ExpiryPolicies::Table,
            ExpiryPolicies::SiteId,
            Sites::Table,
            Sites::Id,
        ))
        .to_owned()
}

fn undo_expiry_policies_table() -> TableCreateStatement {
    let mut table = create_table(UndoExpiryPolicies::Table);
    table
        .col(text_nn(UndoExpiryPolicies::Token))
        .col(text_nn(UndoExpiryPolicies::Path))
        .col(int_nn(UndoExpiryPolicies::TargetKind))
        .col(int_nn(UndoExpiryPolicies::Mode));
    add_undo_expiry_columns(&mut table);
    table
        .primary_key(&mut primary_key([
            UndoExpiryPolicies::Token,
            UndoExpiryPolicies::Path,
        ]))
        .foreign_key(&mut fk_cascade(
            UndoExpiryPolicies::Table,
            UndoExpiryPolicies::Token,
            UndoOperations::Table,
            UndoOperations::Token,
        ))
        .to_owned()
}

fn idempotency_records_table() -> TableCreateStatement {
    create_table(IdempotencyRecords::Table)
        .col(text(IdempotencyRecords::KeyHash).primary_key())
        .col(text_nn(IdempotencyRecords::Fingerprint))
        .col(int_nn(IdempotencyRecords::OperationKind))
        .col(text_nn(IdempotencyRecords::ResultMetadata))
        .col(int_nn(IdempotencyRecords::Expires))
        .to_owned()
}

fn management_tombstones_table() -> TableCreateStatement {
    create_table(ManagementTombstones::Table)
        .col(text(ManagementTombstones::Name).primary_key())
        .col(blob_nn(ManagementTombstones::ManagementHash))
        .col(int_nn(ManagementTombstones::Created))
        .to_owned()
}

fn management_audit_table() -> TableCreateStatement {
    create_table(ManagementAudit::Table)
        .col(int(ManagementAudit::Id).primary_key())
        .col(text_nn(ManagementAudit::SiteName))
        .col(int_nn(ManagementAudit::Action))
        .col(int_nn(ManagementAudit::Occurred))
        .col(text(ManagementAudit::SourceIp))
        .to_owned()
}

fn management_idempotency_table() -> TableCreateStatement {
    create_table(ManagementIdempotency::Table)
        .col(text(ManagementIdempotency::KeyHash).primary_key())
        .col(text_nn(ManagementIdempotency::Fingerprint))
        .col(int_nn(ManagementIdempotency::Expires))
        .to_owned()
}

fn path_aggregates_table() -> TableCreateStatement {
    create_table(PathAggregates::Table)
        .col(int_nn(PathAggregates::SiteId))
        .col(text_nn(PathAggregates::Path))
        .col(int_nn(PathAggregates::LogicalBytes))
        .col(int_nn(PathAggregates::FileCount))
        .primary_key(&mut primary_key([
            PathAggregates::SiteId,
            PathAggregates::Path,
        ]))
        .foreign_key(&mut fk_cascade(
            PathAggregates::Table,
            PathAggregates::SiteId,
            Sites::Table,
            Sites::Id,
        ))
        .to_owned()
}

fn undo_file_deltas_table(layout: Layout) -> TableCreateStatement {
    let mut table = create_table(UndoFileDeltas::Table);
    table
        .col(text_nn(UndoFileDeltas::Token))
        .col(text_nn(UndoFileDeltas::Path))
        .col(int_nn(UndoFileDeltas::Existed))
        .col(int(UndoFileDeltas::Kind))
        .col(layout.hash_column(UndoFileDeltas::Hash))
        .col(int(UndoFileDeltas::Size));
    if layout.modified {
        table.col(undo_modified_column(UndoFileDeltas::Modified));
    }
    table
        .primary_key(&mut primary_key([
            UndoFileDeltas::Token,
            UndoFileDeltas::Path,
        ]))
        .foreign_key(&mut fk_cascade(
            UndoFileDeltas::Table,
            UndoFileDeltas::Token,
            UndoOperations::Table,
            UndoOperations::Token,
        ))
        .to_owned()
}

fn allocated_entries_table(layout: Layout) -> TableCreateStatement {
    let mut table = create_table(AllocatedEntries::Table);
    table
        .col(int_nn(AllocatedEntries::SiteId))
        .col(text_nn(AllocatedEntries::Path))
        .col(
            int_nn(AllocatedEntries::Kind)
                .default(ALLOCATED_ENTRY_KIND)
                .check(Expr::col(AllocatedEntries::Kind).eq(ALLOCATED_ENTRY_KIND)),
        )
        .col(layout.hash_column(AllocatedEntries::Hash).not_null())
        .col(int_nn(AllocatedEntries::Size))
        .col(int_nn(AllocatedEntries::NamingMode))
        .col(text_nn(AllocatedEntries::Prefix))
        .col(text_nn(AllocatedEntries::Suffix))
        .col(text(AllocatedEntries::Extension))
        .col(text_nn(AllocatedEntries::MediaType));
    if layout.modified {
        table.col(modified_column(AllocatedEntries::Modified));
    }
    table
        .primary_key(&mut primary_key([
            AllocatedEntries::SiteId,
            AllocatedEntries::Path,
        ]))
        .foreign_key(&mut site_entry_fk(
            AllocatedEntries::Table,
            AllocatedEntries::SiteId,
            AllocatedEntries::Path,
            AllocatedEntries::Kind,
        ))
        .foreign_key(&mut fk(
            AllocatedEntries::Table,
            AllocatedEntries::Hash,
            Blobs::Table,
            Blobs::Hash,
        ))
        .to_owned()
}

/// `pending_allocations` never had a `modified` column.
fn pending_allocations_table(layout: Layout) -> TableCreateStatement {
    create_table(PendingAllocations::Table)
        .col(text(PendingAllocations::Token).primary_key())
        .col(int_nn(PendingAllocations::SiteId))
        .col(text_nn(PendingAllocations::Folder))
        .col(layout.hash_column(PendingAllocations::Hash).not_null())
        .col(int_nn(PendingAllocations::Size))
        .col(text_nn(PendingAllocations::MediaType))
        .col(text_nn(PendingAllocations::RequestFingerprint))
        .col(int_nn(PendingAllocations::Created))
        .col(int_nn(PendingAllocations::Expires))
        .foreign_key(&mut fk_cascade(
            PendingAllocations::Table,
            PendingAllocations::SiteId,
            Sites::Table,
            Sites::Id,
        ))
        .foreign_key(&mut fk(
            PendingAllocations::Table,
            PendingAllocations::Hash,
            Blobs::Table,
            Blobs::Hash,
        ))
        .to_owned()
}

fn undo_allocated_deltas_table(layout: Layout) -> TableCreateStatement {
    let mut table = create_table(UndoAllocatedDeltas::Table);
    table
        .col(text_nn(UndoAllocatedDeltas::Token))
        .col(text_nn(UndoAllocatedDeltas::Path))
        .col(int_nn(UndoAllocatedDeltas::Existed))
        .col(layout.hash_column(UndoAllocatedDeltas::Hash))
        .col(int(UndoAllocatedDeltas::Size))
        .col(int(UndoAllocatedDeltas::NamingMode))
        .col(text(UndoAllocatedDeltas::Prefix))
        .col(text(UndoAllocatedDeltas::Suffix))
        .col(text(UndoAllocatedDeltas::Extension))
        .col(text(UndoAllocatedDeltas::MediaType));
    if layout.modified {
        table.col(undo_modified_column(UndoAllocatedDeltas::Modified));
    }
    table
        .primary_key(&mut primary_key([
            UndoAllocatedDeltas::Token,
            UndoAllocatedDeltas::Path,
        ]))
        .foreign_key(&mut fk_cascade(
            UndoAllocatedDeltas::Table,
            UndoAllocatedDeltas::Token,
            UndoOperations::Table,
            UndoOperations::Token,
        ))
        .to_owned()
}

fn aliases_table(layout: Layout) -> TableCreateStatement {
    let mut table = create_table(Aliases::Table);
    table
        .col(int_nn(Aliases::SiteId))
        .col(text_nn(Aliases::Path))
        .col(
            int_nn(Aliases::Kind)
                .default(ALIAS_ENTRY_KIND)
                .check(Expr::col(Aliases::Kind).eq(ALIAS_ENTRY_KIND)),
        )
        .col(text_nn(Aliases::CanonicalTarget))
        .col(int(Aliases::ResolvedKind))
        .col(layout.hash_column(Aliases::ResolvedHash))
        .col(int(Aliases::ResolvedSize));
    if layout.modified {
        table.col(modified_column(Aliases::Modified));
    }
    table
        .primary_key(&mut primary_key([Aliases::SiteId, Aliases::Path]))
        .foreign_key(&mut site_entry_fk(
            Aliases::Table,
            Aliases::SiteId,
            Aliases::Path,
            Aliases::Kind,
        ))
        .to_owned()
}

fn undo_alias_deltas_table(layout: Layout) -> TableCreateStatement {
    let mut table = create_table(UndoAliasDeltas::Table);
    table
        .col(text_nn(UndoAliasDeltas::Token))
        .col(text_nn(UndoAliasDeltas::Path))
        .col(int_nn(UndoAliasDeltas::Existed))
        .col(text(UndoAliasDeltas::CanonicalTarget))
        .col(int(UndoAliasDeltas::ResolvedKind))
        .col(layout.hash_column(UndoAliasDeltas::ResolvedHash))
        .col(int(UndoAliasDeltas::ResolvedSize));
    if layout.modified {
        table.col(undo_modified_column(UndoAliasDeltas::Modified));
    }
    table
        .primary_key(&mut primary_key([
            UndoAliasDeltas::Token,
            UndoAliasDeltas::Path,
        ]))
        .foreign_key(&mut fk_cascade(
            UndoAliasDeltas::Table,
            UndoAliasDeltas::Token,
            UndoOperations::Table,
            UndoOperations::Token,
        ))
        .to_owned()
}

fn add_expiry_columns(table: &mut TableCreateStatement) {
    table
        .col(int(ExpiryPolicies::DurationSeconds))
        .col(int(ExpiryPolicies::Deadline))
        .col(int(ExpiryPolicies::MinAgeSeconds))
        .col(int(ExpiryPolicies::MaxAgeSeconds))
        .col(int(ExpiryPolicies::MaxSizeBytes))
        .col(real(ExpiryPolicies::Power))
        .col(int(ExpiryPolicies::Refreshed))
        .col(int(ExpiryPolicies::OwnDeadline))
        .col(int_nn_zero(ExpiryPolicies::SizeBytes));
}

fn add_undo_expiry_columns(table: &mut TableCreateStatement) {
    table
        .col(int(UndoExpiryPolicies::DurationSeconds))
        .col(int(UndoExpiryPolicies::Deadline))
        .col(int(UndoExpiryPolicies::MinAgeSeconds))
        .col(int(UndoExpiryPolicies::MaxAgeSeconds))
        .col(int(UndoExpiryPolicies::MaxSizeBytes))
        .col(real(UndoExpiryPolicies::Power))
        .col(int(UndoExpiryPolicies::Refreshed))
        .col(int_nn(UndoExpiryPolicies::OwnDeadline))
        .col(int_nn(UndoExpiryPolicies::SizeBytes));
}

// Column builders. `_nn` adds `NOT NULL`; `int_nn_zero` is `NOT NULL DEFAULT 0`.

fn int(iden: impl IntoIden) -> ColumnDef {
    ColumnDef::new(iden).integer().to_owned()
}

fn int_nn(iden: impl IntoIden) -> ColumnDef {
    ColumnDef::new(iden).integer().not_null().to_owned()
}

fn int_nn_zero(iden: impl IntoIden) -> ColumnDef {
    ColumnDef::new(iden)
        .integer()
        .not_null()
        .default(0)
        .to_owned()
}

fn text(iden: impl IntoIden) -> ColumnDef {
    ColumnDef::new(iden).text().to_owned()
}

fn text_nn(iden: impl IntoIden) -> ColumnDef {
    ColumnDef::new(iden).text().not_null().to_owned()
}

fn blob(iden: impl IntoIden) -> ColumnDef {
    ColumnDef::new(iden).blob().to_owned()
}

fn blob_nn(iden: impl IntoIden) -> ColumnDef {
    ColumnDef::new(iden).blob().not_null().to_owned()
}

fn real(iden: impl IntoIden) -> ColumnDef {
    ColumnDef::new(iden).custom(Alias::new("real")).to_owned()
}

// Constraint builders.

fn create_table(table: impl IntoTableRef) -> TableCreateStatement {
    Table::create().table(table).if_not_exists().to_owned()
}

fn primary_key<C: IntoIden, const N: usize>(columns: [C; N]) -> IndexCreateStatement {
    let mut key = Index::create();
    for column in columns {
        key.col(column);
    }
    key.take()
}

fn fk(
    from_table: impl IntoTableRef,
    from_column: impl IntoIden,
    to_table: impl IntoTableRef,
    to_column: impl IntoIden,
) -> ForeignKeyCreateStatement {
    ForeignKey::create()
        .from(from_table, from_column)
        .to(to_table, to_column)
        .to_owned()
}

/// A foreign key whose rows go away with the row they reference.
fn fk_cascade(
    from_table: impl IntoTableRef,
    from_column: impl IntoIden,
    to_table: impl IntoTableRef,
    to_column: impl IntoIden,
) -> ForeignKeyCreateStatement {
    fk(from_table, from_column, to_table, to_column)
        .on_delete(ForeignKeyAction::Cascade)
        .to_owned()
}

/// The composite key tying an entry to its `site_entries` row, which is what
/// keeps a path from being a file and an alias at once.
fn site_entry_fk(
    table: impl IntoTableRef,
    site_id: impl IntoIden,
    path: impl IntoIden,
    kind: impl IntoIden,
) -> ForeignKeyCreateStatement {
    ForeignKey::create()
        .from_tbl(table)
        .from_col(site_id)
        .from_col(path)
        .from_col(kind)
        .to_tbl(SiteEntries::Table)
        .to_col(SiteEntries::SiteId)
        .to_col(SiteEntries::Path)
        .to_col(SiteEntries::Kind)
        .on_delete(ForeignKeyAction::Cascade)
        .to_owned()
}

// Index builders.

fn plain_index<T, C, const N: usize>(name: &str, table: T, columns: [C; N]) -> IndexCreateStatement
where
    T: IntoTableRef,
    C: IntoIden,
{
    let mut index = Index::create();
    index.name(name).table(table);
    for column in columns {
        index.col(column);
    }
    index.take()
}

fn index<T, C, const N: usize>(name: &str, table: T, columns: [C; N]) -> IndexCreateStatement
where
    T: IntoTableRef,
    C: IntoIden,
{
    let mut index = plain_index(name, table, columns);
    index.if_not_exists();
    index
}

/// The entry of [`indexes`] called `name`.
fn named_index(name: &str) -> IndexCreateStatement {
    indexes()
        .into_iter()
        .find(|index| index.get_index_spec().get_name() == Some(name))
        .unwrap_or_else(|| panic!("no index named {name}"))
}

fn named_index_sql(name: &str) -> String {
    to_sql(&named_index(name))
}

fn named_index_sqls(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| named_index_sql(name)).collect()
}

// Statement builders.

fn to_sql(statement: &impl SchemaStatementBuilder) -> String {
    statement.to_string(SqliteQueryBuilder)
}

fn rename(from: impl IntoTableRef, to: impl IntoTableRef) -> String {
    to_sql(Table::rename().table(from, to))
}

fn drop_table(table: impl IntoTableRef) -> String {
    to_sql(Table::drop().table(table))
}

fn add_column(table: impl IntoTableRef, mut column: ColumnDef) -> String {
    to_sql(Table::alter().table(table).add_column(&mut column))
}

#[cfg(test)]
fn drop_column(table: impl IntoTableRef, column: impl IntoIden) -> String {
    to_sql(Table::alter().table(table).drop_column(column))
}

#[cfg(test)]
fn drop_index(name: &str, table: impl IntoTableRef) -> String {
    to_sql(Index::drop().name(name).table(table))
}

/// Moves `table` aside as `old`, then creates its replacement and copies the
/// rows across with `copy`. The caller drops `old` once nothing else needs it.
fn recreate_table(
    table: &str,
    old: &str,
    create: &TableCreateStatement,
    copy: impl Into<String>,
) -> Vec<String> {
    vec![
        rename(Alias::new(table), Alias::new(old)),
        to_sql(create),
        copy.into(),
    ]
}

/// [`recreate_table`], then drops `old`: the way to change a table's shape
/// when `ALTER TABLE` cannot.
fn rebuild_table(
    table: &str,
    old: &str,
    create: &TableCreateStatement,
    copy: impl Into<String>,
) -> Vec<String> {
    let mut statements = recreate_table(table, old, create, copy);
    statements.push(drop_table(Alias::new(old)));
    statements
}

#[cfg(test)]
mod tests {
    use super::schema_sql;

    #[test]
    fn generated_schema_snapshot_is_current() {
        assert_eq!(
            schema_sql(),
            include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../schema.sql"))
        );
    }
}
