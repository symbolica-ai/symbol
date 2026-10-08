// Small queries that several store operations repeat verbatim.
use diesel::dsl::count_star;
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;

use super::{MANIFEST_PATH, StoreError, site_exists_locked};
use crate::database::schema::ALIAS_ENTRY_KIND;
use crate::schema::{site_entries, sites};

/// Number of file entries (regular and allocated) in a site, excluding the
/// generated manifest and aliases.
pub(super) fn count_files_locked(
    tx: &mut SqliteConnection,
    site_id: i64,
) -> Result<usize, diesel::result::Error> {
    let files = site_entries::table
        .filter(site_entries::site_id.eq(site_id))
        .filter(site_entries::path.ne(MANIFEST_PATH))
        .filter(site_entries::kind.ne(ALIAS_ENTRY_KIND))
        .select(count_star())
        .first::<i64>(tx)?;
    Ok(usize::try_from(files).expect("file count fits in usize"))
}

/// `NotFound` unless the site exists.
pub(super) fn require_site_locked(db: &mut SqliteConnection, name: &str) -> Result<(), StoreError> {
    if site_exists_locked(db, name)? {
        Ok(())
    } else {
        Err(StoreError::NotFound)
    }
}

pub(super) fn bump_revision_locked(
    tx: &mut SqliteConnection,
    site_id: i64,
) -> Result<(), diesel::result::Error> {
    diesel::update(sites::table.find(site_id))
        .set(sites::content_revision.eq(sites::content_revision + 1))
        .execute(tx)?;
    Ok(())
}

/// Bump the revision and stamp the site as updated at `now`.
pub(super) fn bump_revision_and_touch_locked(
    tx: &mut SqliteConnection,
    site_id: i64,
    now: i64,
) -> Result<(), diesel::result::Error> {
    diesel::update(sites::table.find(site_id))
        .set((
            sites::updated.eq(now),
            sites::content_revision.eq(sites::content_revision + 1),
        ))
        .execute(tx)?;
    Ok(())
}
