//! The one database schema this build supports: version 12.
//!
//! The DDL lives in `schema.v12.sql`, hand-maintained and byte-for-byte the
//! text the earlier DDL builders generated. Its BLAKE3 hash is stored in every
//! database's `schema.migration.v12` record, so editing the file by accident
//! locks existing databases out; the tripwire test in `migrations` pins it.
//! The root `schema.sql` is a snapshot of the same bytes that `build.rs`
//! compares against in read-only mode. Its header names this file because the
//! snapshot predates the move to plain SQL.
//!
//! Upgrading older databases is no longer supported. A database below version
//! 12 must first be opened by a build at or before commit
//! [`LEGACY_UPGRADE_COMMIT`] (`5098872`), which migrates it to version 12,
//! before this build can open it.
//!
//! `build.rs` and `examples/schema.rs` include this file with `#[path]`, so it
//! must not depend on anything else in the crate.

pub const LATEST_SCHEMA_VERSION: i64 = 12;
/// The last revision that can upgrade a database below [`LATEST_SCHEMA_VERSION`].
pub const LEGACY_UPGRADE_COMMIT: &str = "5098872";
pub const FILE_ENTRY_KIND: i64 = 0;
pub const ALLOCATED_ENTRY_KIND: i64 = 1;
pub const ALIAS_ENTRY_KIND: i64 = 2;

/// The DDL that creates a version 12 database from nothing.
pub fn schema_sql() -> String {
    include_str!("schema.v12.sql").to_owned()
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
