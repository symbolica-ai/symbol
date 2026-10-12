use diesel::connection::SimpleConnection;
use diesel::prelude::*;
use diesel::sql_types::BigInt;
use diesel::sqlite::SqliteConnection;
use serde::{Deserialize, Serialize};

use super::schema;
use crate::schema::metadata;

#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    #[error(
        "unsupported database schema version {0}: this build opens only schema version \
         {latest} (and creates new databases at it); a database below that version must \
         first be opened by a build at or before commit {commit} to migrate it, and only \
         then upgraded to this build",
        latest = schema::LATEST_SCHEMA_VERSION,
        commit = schema::LEGACY_UPGRADE_COMMIT
    )]
    UnsupportedVersion(i64),
    #[error(transparent)]
    Connection(#[from] diesel::ConnectionError),
    #[error(transparent)]
    Database(#[from] diesel::result::Error),
    #[error("invalid migration metadata: {0}")]
    Metadata(String),
}

#[derive(QueryableByName)]
struct SchemaVersion {
    #[diesel(sql_type = BigInt)]
    user_version: i64,
}

#[derive(Debug, Deserialize, Serialize)]
struct MigrationRecord {
    version: i64,
    schema_hash: String,
    program: MigrationProgram,
    program_hash: String,
    source_revision: u64,
    applied_unix_seconds: u64,
}

/// How a version 12 database got there, as recorded in its migration record.
///
/// Only `FreshV12` and `BaselineV12` can be written by this build. The rest
/// are kept so that a database already upgraded by an earlier build still
/// deserializes and validates; see [`migration_program_hash`].
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum MigrationProgram {
    FreshV12,
    V2ToV12,
    V6ToV12,
    V7ToV12,
    V8ToV12,
    V9ToV12,
    V10ToV12,
    V11ToV12,
    BaselineV12,
}

/// Creates a new database at the latest schema, or accepts one already there.
///
/// Every other version is refused: this build carries no upgrade code.
pub fn migrate(db: &mut SqliteConnection) -> Result<(), MigrationError> {
    match schema_version(db)? {
        0 => db.transaction::<_, MigrationError, _>(|connection| {
            execute(connection, &schema::schema_sql())?;
            ensure_migration_record(connection, MigrationProgram::FreshV12)
        }),
        schema::LATEST_SCHEMA_VERSION => db.transaction::<_, MigrationError, _>(|connection| {
            ensure_migration_record(connection, MigrationProgram::BaselineV12)
        }),
        unsupported => Err(MigrationError::UnsupportedVersion(unsupported)),
    }
}

pub fn schema_version(db: &mut SqliteConnection) -> Result<i64, diesel::result::Error> {
    diesel::sql_query("PRAGMA user_version")
        .get_result::<SchemaVersion>(db)
        .map(|version| version.user_version)
}

fn execute(db: &mut SqliteConnection, statement: &str) -> Result<(), MigrationError> {
    db.batch_execute(statement)?;
    Ok(())
}

/// Record keys written by earlier generations of this migration engine.
///
/// [`ensure_migration_record`] writes `schema.migration.v{LATEST_SCHEMA_VERSION}`
/// and that key is renamed on every schema bump, so each previous key is stale
/// once a database reaches the current version. Removing them centrally keeps
/// the list from being re-derived, and mis-derived, in each `migrate` arm.
const SUPERSEDED_MIGRATION_RECORD_KEYS: [&str; 6] = [
    "schema.migration.v6",
    "schema.migration.v7",
    "schema.migration.v8",
    "schema.migration.v9",
    "schema.migration.v10",
    "schema.migration.v11",
];

fn ensure_migration_record(
    db: &mut SqliteConnection,
    executed_program: MigrationProgram,
) -> Result<(), MigrationError> {
    const KEY: &str = "schema.migration.v12";
    debug_assert!(
        !SUPERSEDED_MIGRATION_RECORD_KEYS.contains(&KEY),
        "the current record key must not be listed as superseded"
    );
    diesel::delete(metadata::table.filter(metadata::key.eq_any(SUPERSEDED_MIGRATION_RECORD_KEYS)))
        .execute(db)?;
    let hash = blake3::hash(schema::schema_sql().as_bytes())
        .to_hex()
        .to_string();
    let existing = metadata::table
        .find(KEY)
        .select(metadata::value)
        .first::<String>(db)
        .optional()?;
    if let Some(existing) = existing {
        let record: MigrationRecord = serde_json::from_str(&existing)
            .map_err(|error| MigrationError::Metadata(error.to_string()))?;
        let expected_program_hash = migration_program_hash(record.program);
        if record.version != schema::LATEST_SCHEMA_VERSION
            || record.schema_hash != hash
            || record.program_hash != expected_program_hash
        {
            return Err(MigrationError::Metadata(format!(
                "schema checksum drift for version {}",
                record.version
            )));
        }
        return Ok(());
    }
    let applied_unix_seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| MigrationError::Metadata(error.to_string()))?
        .as_secs();
    let record = MigrationRecord {
        version: schema::LATEST_SCHEMA_VERSION,
        schema_hash: hash,
        program: executed_program,
        program_hash: migration_program_hash(executed_program),
        source_revision: source_revision(),
        applied_unix_seconds,
    };
    let value = serde_json::to_string(&record)
        .map_err(|error| MigrationError::Metadata(error.to_string()))?;
    diesel::insert_into(metadata::table)
        .values((metadata::key.eq(KEY), metadata::value.eq(value)))
        .execute(db)?;
    Ok(())
}

/// The API revision of the build that applied a migration.
///
/// Recorded for provenance only. `ensure_migration_record` compares the schema
/// and program hashes, never this, so records written by older builds that
/// hardcoded `0` stay valid.
fn source_revision() -> u64 {
    env!("SYMBOL_API_REVISION").parse().unwrap_or(0)
}

/// Program hashes of the retired upgrade programs, frozen at their values.
///
/// A database upgraded by an earlier build stores one of these in its
/// `schema.migration.v12` record, and [`ensure_migration_record`] must keep
/// accepting it. Earlier builds derived each from the upgrade statements they
/// ran (the hash of those statements joined with `;\n`, ending in
/// `PRAGMA user_version = 12`, after a `baseline-vN` or `validated-v6` prefix).
/// Those statements are gone, so the results are pinned here, taken from the
/// code at commit `5098872` before it was deleted. They must never change.
const V2_TO_V12_PROGRAM_HASH: &str =
    "f4ffcaccc56dea792cadc11c1cb0e015f3a9538a49200055190ac4cdc0b5ce62";
const V6_TO_V12_PROGRAM_HASH: &str =
    "cdb2343bff38fa5a952cfa495452cd855694930694e3745a9a74608b22af564b";
const V7_TO_V12_PROGRAM_HASH: &str =
    "05d0aaa7fb7a9e67cb025f753569ab30b31c6e769d401b66151bc8f4e32c1b7a";
const V8_TO_V12_PROGRAM_HASH: &str =
    "5371830285975ea9104742ec8db08569dc1ebaf8a854b22dd5825274a51eac23";
const V9_TO_V12_PROGRAM_HASH: &str =
    "09dbda53890bab80c352302a7e2fc700625ca21af4b91245f8348100839e93e7";
const V10_TO_V12_PROGRAM_HASH: &str =
    "5258ecd5cb3cc0a30ab1b28a58209b11fcc718db142d23dc9a1f5542b3302883";
const V11_TO_V12_PROGRAM_HASH: &str =
    "7fe93b41b7ecd937bf39157481cfeb654cc454eb503a774b490139655fc0db36";

/// The hash recorded for `program`.
///
/// The two programs this build can still run are derived from the schema text;
/// the retired upgrade programs are the frozen constants above.
fn migration_program_hash(program: MigrationProgram) -> String {
    let source = match program {
        MigrationProgram::FreshV12 => schema::schema_sql(),
        MigrationProgram::BaselineV12 => format!("baseline-v12\n{}", schema::schema_sql()),
        MigrationProgram::V2ToV12 => return V2_TO_V12_PROGRAM_HASH.to_owned(),
        MigrationProgram::V6ToV12 => return V6_TO_V12_PROGRAM_HASH.to_owned(),
        MigrationProgram::V7ToV12 => return V7_TO_V12_PROGRAM_HASH.to_owned(),
        MigrationProgram::V8ToV12 => return V8_TO_V12_PROGRAM_HASH.to_owned(),
        MigrationProgram::V9ToV12 => return V9_TO_V12_PROGRAM_HASH.to_owned(),
        MigrationProgram::V10ToV12 => return V10_TO_V12_PROGRAM_HASH.to_owned(),
        MigrationProgram::V11ToV12 => return V11_TO_V12_PROGRAM_HASH.to_owned(),
    };
    blake3::hash(source.as_bytes()).to_hex().to_string()
}

#[cfg(test)]
mod tests {
    use diesel::Connection;
    use diesel::sql_types::{BigInt, Text};

    use super::*;
    use crate::hash::{ContentHash, TreeHash};
    use crate::schema::{blobs, files, site_entries, sites};

    const KEY: &str = "schema.migration.v12";

    /// `blake3(schema_sql())` as stored in every deployed database's record.
    const SCHEMA_V12_BLAKE3: &str =
        "2cbcb53662620a13ee85d1d736953d0c36f7701a411b465695be891040a39cf0";

    /// Every program a version 12 record may name, as serialized in a stored
    /// record, with the hash those records carry. The literals were taken from
    /// the code at commit `5098872`, so a change to either side is caught.
    const STORED_PROGRAMS: [(&str, MigrationProgram, &str); 9] = [
        (
            "fresh-v12",
            MigrationProgram::FreshV12,
            "2cbcb53662620a13ee85d1d736953d0c36f7701a411b465695be891040a39cf0",
        ),
        (
            "v2-to-v12",
            MigrationProgram::V2ToV12,
            "f4ffcaccc56dea792cadc11c1cb0e015f3a9538a49200055190ac4cdc0b5ce62",
        ),
        (
            "v6-to-v12",
            MigrationProgram::V6ToV12,
            "cdb2343bff38fa5a952cfa495452cd855694930694e3745a9a74608b22af564b",
        ),
        (
            "v7-to-v12",
            MigrationProgram::V7ToV12,
            "05d0aaa7fb7a9e67cb025f753569ab30b31c6e769d401b66151bc8f4e32c1b7a",
        ),
        (
            "v8-to-v12",
            MigrationProgram::V8ToV12,
            "5371830285975ea9104742ec8db08569dc1ebaf8a854b22dd5825274a51eac23",
        ),
        (
            "v9-to-v12",
            MigrationProgram::V9ToV12,
            "09dbda53890bab80c352302a7e2fc700625ca21af4b91245f8348100839e93e7",
        ),
        (
            "v10-to-v12",
            MigrationProgram::V10ToV12,
            "5258ecd5cb3cc0a30ab1b28a58209b11fcc718db142d23dc9a1f5542b3302883",
        ),
        (
            "v11-to-v12",
            MigrationProgram::V11ToV12,
            "7fe93b41b7ecd937bf39157481cfeb654cc454eb503a774b490139655fc0db36",
        ),
        (
            "baseline-v12",
            MigrationProgram::BaselineV12,
            "31427ad52d2305ab92fdd4f756c177e772fa8601a91781b10b9fec2d7728e6f9",
        ),
    ];

    #[derive(Insertable)]
    #[diesel(table_name = files)]
    struct MigratedFile {
        site_id: i64,
        path: &'static str,
        kind: i64,
        hash: ContentHash,
        size: i64,
        modified: i64,
    }

    fn test_content_hash(label: &str) -> ContentHash {
        ContentHash::from(blake3::hash(label.as_bytes()))
    }

    #[derive(QueryableByName)]
    struct SchemaObject {
        #[diesel(column_name = "type")]
        #[diesel(sql_type = Text)]
        object_type: String,
        #[diesel(sql_type = Text)]
        name: String,
        #[diesel(sql_type = Text)]
        tbl_name: String,
    }

    #[derive(QueryableByName, Debug, PartialEq, Eq)]
    struct ForeignKeyColumn {
        #[diesel(sql_type = Text)]
        table: String,
        #[diesel(sql_type = Text)]
        from: String,
        #[diesel(sql_type = Text)]
        to: String,
    }

    #[derive(QueryableByName)]
    struct ObjectCount {
        #[diesel(sql_type = BigInt)]
        count: i64,
    }

    fn connection() -> (tempfile::TempDir, SqliteConnection) {
        let root = tempfile::tempdir().unwrap();
        let database = root.path().join("symbol.db");
        let connection = SqliteConnection::establish(&database.to_string_lossy()).unwrap();
        (root, connection)
    }

    fn stored_record(db: &mut SqliteConnection) -> String {
        metadata::table
            .find(KEY)
            .select(metadata::value)
            .first::<String>(db)
            .unwrap()
    }

    fn overwrite_record(db: &mut SqliteConnection, value: &str) {
        diesel::update(metadata::table.find(KEY))
            .set(metadata::value.eq(value))
            .execute(db)
            .unwrap();
    }

    /// A record as a deployed database stores it, built from literals rather
    /// than the `Serialize` impl so the on-disk format is pinned too.
    fn raw_record(version: i64, schema_hash: &str, program: &str, program_hash: &str) -> String {
        format!(
            r#"{{"version":{version},"schema_hash":"{schema_hash}","program":"{program}","program_hash":"{program_hash}","source_revision":0,"applied_unix_seconds":1700000000}}"#
        )
    }

    fn assert_checksum_drift(db: &mut SqliteConnection, version: i64) {
        assert!(
            matches!(
                migrate(db),
                Err(MigrationError::Metadata(message))
                    if message == format!("schema checksum drift for version {version}")
            ),
            "record was not rejected as drift"
        );
    }

    fn object_count(db: &mut SqliteConnection) -> i64 {
        diesel::sql_query("SELECT COUNT(*) AS count FROM sqlite_schema")
            .get_result::<ObjectCount>(db)
            .unwrap()
            .count
    }

    #[test]
    fn schema_sql_is_pinned_to_the_deployed_text() {
        assert_eq!(
            blake3::hash(schema::schema_sql().as_bytes())
                .to_hex()
                .as_str(),
            SCHEMA_V12_BLAKE3,
            "schema.v12.sql changed: every deployed database stores this hash"
        );
    }

    #[test]
    fn fresh_database_gets_the_v12_schema_and_a_fresh_record() {
        let (_root, mut db) = connection();
        migrate(&mut db).unwrap();

        assert_eq!(schema_version(&mut db).unwrap(), 12);
        let record: MigrationRecord = serde_json::from_str(&stored_record(&mut db)).unwrap();
        assert_eq!(record.version, 12);
        assert_eq!(record.program, MigrationProgram::FreshV12);
        assert_eq!(record.schema_hash, SCHEMA_V12_BLAKE3);
        assert_eq!(record.program_hash, SCHEMA_V12_BLAKE3);
        assert_eq!(record.source_revision, source_revision());
    }

    #[test]
    fn reopening_is_idempotent() {
        let (_root, mut db) = connection();
        migrate(&mut db).unwrap();
        let first = stored_record(&mut db);
        let objects = object_count(&mut db);

        migrate(&mut db).unwrap();
        migrate(&mut db).unwrap();

        assert_eq!(stored_record(&mut db), first);
        assert_eq!(object_count(&mut db), objects);
        assert_eq!(schema_version(&mut db).unwrap(), 12);

        // A v12 database without a record, as an early v12 build left it, is
        // adopted as a baseline.
        diesel::delete(metadata::table.find(KEY))
            .execute(&mut db)
            .unwrap();
        migrate(&mut db).unwrap();
        let record: MigrationRecord = serde_json::from_str(&stored_record(&mut db)).unwrap();
        assert_eq!(record.program, MigrationProgram::BaselineV12);
        assert_ne!(record.program_hash, SCHEMA_V12_BLAKE3);
    }

    #[test]
    fn every_v12_program_is_accepted_with_its_frozen_hash() {
        for (name, program, hash) in STORED_PROGRAMS {
            assert_eq!(migration_program_hash(program), hash, "{name}");
            assert_eq!(
                serde_json::to_string(&program).unwrap(),
                format!("\"{name}\"")
            );

            let (_root, mut db) = connection();
            migrate(&mut db).unwrap();
            let stored = raw_record(12, SCHEMA_V12_BLAKE3, name, hash);
            overwrite_record(&mut db, &stored);

            migrate(&mut db).unwrap_or_else(|error| panic!("{name} was rejected: {error}"));
            assert_eq!(stored_record(&mut db), stored, "{name} was rewritten");
        }
    }

    #[test]
    fn records_that_do_not_match_are_rejected_as_checksum_drift() {
        let wrong_hash = "0".repeat(64);
        for (index, (name, _, hash)) in STORED_PROGRAMS.iter().enumerate() {
            // Another program's hash, and one that matches nothing.
            let other_hash = STORED_PROGRAMS[(index + 1) % STORED_PROGRAMS.len()].2;
            for (version, schema_hash, program_hash) in [
                (12, SCHEMA_V12_BLAKE3, other_hash),
                (12, SCHEMA_V12_BLAKE3, wrong_hash.as_str()),
                (12, wrong_hash.as_str(), hash),
                (11, SCHEMA_V12_BLAKE3, hash),
            ] {
                let (_root, mut db) = connection();
                migrate(&mut db).unwrap();
                overwrite_record(
                    &mut db,
                    &raw_record(version, schema_hash, name, program_hash),
                );
                assert_checksum_drift(&mut db, version);
            }
        }
    }

    #[test]
    fn programs_that_cannot_appear_in_a_v12_record_are_rejected() {
        for name in ["fresh-v6", "v2-to-v6", "baseline-v6", "v12-to-v13"] {
            let (_root, mut db) = connection();
            migrate(&mut db).unwrap();
            overwrite_record(
                &mut db,
                &raw_record(12, SCHEMA_V12_BLAKE3, name, SCHEMA_V12_BLAKE3),
            );
            assert!(
                matches!(migrate(&mut db), Err(MigrationError::Metadata(_))),
                "{name} was accepted"
            );
        }
    }

    #[test]
    fn older_versions_are_refused_with_upgrade_instructions() {
        for version in [2, 6, 11, 99] {
            let (_root, mut db) = connection();
            db.batch_execute(&format!("PRAGMA user_version = {version}"))
                .unwrap();

            let error = migrate(&mut db).unwrap_err();

            assert!(
                matches!(error, MigrationError::UnsupportedVersion(found) if found == version),
                "{error:?}"
            );
            let message = error.to_string();
            assert!(
                message.contains(&format!("schema version {version}"))
                    && message.contains("5098872")
                    && message.contains("version 12"),
                "{message}"
            );
            assert_eq!(schema_version(&mut db).unwrap(), version);
            assert_eq!(object_count(&mut db), 0, "a refused database was touched");
        }
    }

    #[test]
    fn fresh_catalog_has_exact_tables_and_indexes() {
        let (_root, mut db) = connection();
        migrate(&mut db).unwrap();
        let objects = diesel::sql_query(
            "SELECT type, name, tbl_name
             FROM sqlite_schema
             WHERE name NOT LIKE 'sqlite_%'
             ORDER BY type, name",
        )
        .load::<SchemaObject>(&mut db)
        .unwrap()
        .into_iter()
        .map(|object| (object.object_type, object.name, object.tbl_name))
        .collect::<Vec<_>>();
        let expected = [
            ("index", "aliases_cache", "aliases"),
            ("index", "aliases_dependency", "aliases"),
            ("index", "expiry_policies_deadline", "expiry_policies"),
            ("index", "expiry_policies_site_kind", "expiry_policies"),
            ("index", "files_hash", "files"),
            ("index", "files_site_hash", "files"),
            ("index", "files_site_prefix", "files"),
            ("index", "idempotency_records_expiry", "idempotency_records"),
            ("index", "management_audit_site", "management_audit"),
            (
                "index",
                "management_idempotency_expiry",
                "management_idempotency",
            ),
            ("index", "pending_allocations_expiry", "pending_allocations"),
            ("index", "site_events_site", "site_events"),
            ("index", "undo_files_hash", "undo_files"),
            ("index", "undo_names_stack", "undo_names"),
            ("index", "undo_operations_retention", "undo_operations"),
            ("table", "aliases", "aliases"),
            ("table", "allocated_entries", "allocated_entries"),
            ("table", "blobs", "blobs"),
            ("table", "expiry_policies", "expiry_policies"),
            ("table", "files", "files"),
            ("table", "idempotency_records", "idempotency_records"),
            ("table", "management_audit", "management_audit"),
            ("table", "management_idempotency", "management_idempotency"),
            ("table", "management_tombstones", "management_tombstones"),
            ("table", "metadata", "metadata"),
            ("table", "path_aggregates", "path_aggregates"),
            ("table", "pending_allocations", "pending_allocations"),
            ("table", "site_entries", "site_entries"),
            ("table", "site_events", "site_events"),
            ("table", "sites", "sites"),
            ("table", "undo_alias_deltas", "undo_alias_deltas"),
            ("table", "undo_allocated_deltas", "undo_allocated_deltas"),
            ("table", "undo_expiry_policies", "undo_expiry_policies"),
            ("table", "undo_file_deltas", "undo_file_deltas"),
            ("table", "undo_files", "undo_files"),
            ("table", "undo_names", "undo_names"),
            ("table", "undo_operations", "undo_operations"),
            ("table", "undo_sites", "undo_sites"),
        ]
        .map(|(object_type, name, table)| {
            (object_type.to_string(), name.to_string(), table.to_string())
        });
        assert_eq!(objects, expected);
        assert_eq!(
            schema_version(&mut db).unwrap(),
            schema::LATEST_SCHEMA_VERSION
        );
        let file_foreign_keys = diesel::sql_query(
            "SELECT \"table\", \"from\", \"to\" FROM pragma_foreign_key_list('files')
             ORDER BY id DESC, seq",
        )
        .load::<ForeignKeyColumn>(&mut db)
        .unwrap();
        assert_eq!(
            file_foreign_keys,
            [
                ForeignKeyColumn {
                    table: "site_entries".to_string(),
                    from: "site_id".to_string(),
                    to: "site_id".to_string(),
                },
                ForeignKeyColumn {
                    table: "site_entries".to_string(),
                    from: "path".to_string(),
                    to: "path".to_string(),
                },
                ForeignKeyColumn {
                    table: "site_entries".to_string(),
                    from: "kind".to_string(),
                    to: "kind".to_string(),
                },
                ForeignKeyColumn {
                    table: "blobs".to_string(),
                    from: "hash".to_string(),
                    to: "hash".to_string(),
                },
            ]
        );
    }

    #[test]
    fn composite_entry_foreign_key_rejects_cross_kind_rows() {
        let (_root, mut db) = connection();
        migrate(&mut db).unwrap();
        diesel::insert_into(sites::table)
            .values((
                sites::name.eq("kinds"),
                sites::updated.eq(0_i64),
                sites::public_url.eq(""),
                sites::content_revision.eq(0_i64),
                sites::tree_hash.eq(TreeHash::EMPTY),
                sites::management_status.eq(0_i64),
            ))
            .execute(&mut db)
            .unwrap();
        let site_id = sites::table
            .select(sites::id)
            .first::<i64>(&mut db)
            .unwrap();
        diesel::insert_into(blobs::table)
            .values((
                blobs::hash.eq(test_content_hash("hash")),
                blobs::bytes.eq(Vec::<u8>::new()),
                blobs::size.eq(1_i64),
            ))
            .execute(&mut db)
            .unwrap();
        diesel::insert_into(site_entries::table)
            .values((
                site_entries::site_id.eq(site_id),
                site_entries::path.eq("reserved"),
                site_entries::kind.eq(schema::ALLOCATED_ENTRY_KIND),
            ))
            .execute(&mut db)
            .unwrap();
        assert!(matches!(
            diesel::insert_into(files::table)
                .values(MigratedFile {
                    site_id,
                    path: "reserved",
                    kind: schema::FILE_ENTRY_KIND,
                    hash: test_content_hash("hash"),
                    size: 1,
                    modified: 0,
                })
                .execute(&mut db),
            Err(diesel::result::Error::DatabaseError(
                diesel::result::DatabaseErrorKind::ForeignKeyViolation,
                _
            ))
        ));
    }

    #[test]
    fn subtype_tables_reject_noncanonical_kinds() {
        let (_root, mut db) = connection();
        migrate(&mut db).unwrap();
        db.batch_execute(
            "INSERT INTO sites
                (name, updated, public_url, content_revision, tree_hash, management_status)
             VALUES ('checks', 0, '', 0, '', 0);
             INSERT INTO blobs (hash, bytes, size) VALUES ('check-hash', X'', 0);
             INSERT INTO site_entries (site_id, path, kind)
             SELECT id, 'bad-file', 1 FROM sites WHERE name = 'checks';
             INSERT INTO site_entries (site_id, path, kind)
             SELECT id, 'bad-allocated', 0 FROM sites WHERE name = 'checks';
             INSERT INTO site_entries (site_id, path, kind)
             SELECT id, 'bad-alias', 0 FROM sites WHERE name = 'checks';",
        )
        .unwrap();
        for statement in [
            "INSERT INTO files (site_id, path, kind, hash, size)
             SELECT id, 'bad-file', 1, 'check-hash', 0 FROM sites WHERE name = 'checks'",
            "INSERT INTO allocated_entries
                (site_id, path, kind, hash, size, naming_mode, prefix, suffix, media_type)
             SELECT id, 'bad-allocated', 0, 'check-hash', 0, 0, '', '', ''
             FROM sites WHERE name = 'checks'",
            "INSERT INTO aliases (site_id, path, kind, canonical_target)
             SELECT id, 'bad-alias', 0, 'target' FROM sites WHERE name = 'checks'",
        ] {
            assert!(
                db.batch_execute(statement).is_err(),
                "schema accepted {statement}"
            );
        }
    }

    #[test]
    fn superseded_migration_records_are_removed_on_every_path() {
        // A database that reached v11 through the historical v10 arm kept its
        // `schema.migration.v10` row, because that arm deleted the current key
        // instead of the superseded one. Reaching v12 again must clean it up.
        for stale in SUPERSEDED_MIGRATION_RECORD_KEYS {
            let (_root, mut db) = connection();
            migrate(&mut db).unwrap();
            diesel::insert_into(metadata::table)
                .values((metadata::key.eq(stale), metadata::value.eq("stale")))
                .execute(&mut db)
                .unwrap();

            migrate(&mut db).unwrap();

            assert_eq!(
                metadata::table
                    .find(stale)
                    .select(metadata::value)
                    .first::<String>(&mut db)
                    .optional()
                    .unwrap(),
                None,
                "{stale} survived"
            );
            assert!(
                metadata::table
                    .find(KEY)
                    .select(metadata::value)
                    .first::<String>(&mut db)
                    .is_ok(),
                "{stale} run dropped the current record"
            );
        }
    }

    #[test]
    fn superseded_cleanup_does_not_mask_current_record_drift() {
        let (_root, mut db) = connection();
        migrate(&mut db).unwrap();
        let mut record: MigrationRecord = serde_json::from_str(&stored_record(&mut db)).unwrap();
        record.program = MigrationProgram::V10ToV12;
        overwrite_record(&mut db, &serde_json::to_string(&record).unwrap());
        diesel::insert_into(metadata::table)
            .values((
                metadata::key.eq("schema.migration.v10"),
                metadata::value.eq("stale"),
            ))
            .execute(&mut db)
            .unwrap();

        assert!(matches!(migrate(&mut db), Err(MigrationError::Metadata(_))));

        // The rollback keeps the stale row, so the repair is not silently lost.
        assert!(
            metadata::table
                .find("schema.migration.v10")
                .select(metadata::value)
                .first::<String>(&mut db)
                .is_ok()
        );
    }
}
