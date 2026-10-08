// The writer-lock transaction every mutating `Store` method runs inside.
use diesel::sqlite::SqliteConnection;

use super::{ContentHash, DbTransaction, Store, StoreError, gc_blobs, prune_undo_locked};

/// How a write transaction ends.
pub(super) enum TxOutcome<T> {
    /// Commit, then quarantine the blob files the transaction unreferenced.
    Commit(T, Vec<ContentHash>),
    /// Roll back and hand the value back. Idempotent replays end this way: the
    /// transaction only read, so none of its bookkeeping is kept.
    Rollback(T),
}

/// The tail shared by mutations that end the undo window and sweep blobs:
/// drop expired or surplus undo operations, then delete unreferenced blobs.
/// Returns the removed hashes for [`Store::write`] to quarantine.
pub(super) fn finish_mutation(
    tx: &mut SqliteConnection,
    now: i64,
) -> Result<Vec<ContentHash>, diesel::result::Error> {
    prune_undo_locked(tx, now)?;
    gc_blobs(tx, now)
}

impl Store {
    /// Run `body` in a write transaction under the writer lock.
    ///
    /// The lock is released before the removed blob files are touched, so
    /// filesystem work never blocks other writers. An error from `body` (or a
    /// failed commit) rolls the transaction back.
    pub(super) fn write_outcome<T>(
        &self,
        body: impl FnOnce(&mut SqliteConnection) -> Result<TxOutcome<T>, StoreError>,
    ) -> Result<T, StoreError> {
        let mut db = self.inner.writer.lock().unwrap();
        let mut tx = DbTransaction::begin(&mut db)?;
        match body(&mut tx)? {
            TxOutcome::Commit(value, removed) => {
                tx.commit()?;
                drop(db);
                self.remove_blob_files(&removed);
                Ok(value)
            }
            TxOutcome::Rollback(value) => {
                drop(tx);
                drop(db);
                Ok(value)
            }
        }
    }

    /// [`Self::write_outcome`] for a body that always commits and reports the
    /// blobs it unreferenced.
    pub(super) fn write<T>(
        &self,
        body: impl FnOnce(&mut SqliteConnection) -> Result<(T, Vec<ContentHash>), StoreError>,
    ) -> Result<T, StoreError> {
        self.write_outcome(|tx| body(tx).map(|(value, removed)| TxOutcome::Commit(value, removed)))
    }

    /// [`Self::write_outcome`] for a body that always commits and leaves no
    /// blobs unreferenced.
    pub(super) fn write_plain<T>(
        &self,
        body: impl FnOnce(&mut SqliteConnection) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.write(|tx| body(tx).map(|value| (value, Vec::new())))
    }
}
