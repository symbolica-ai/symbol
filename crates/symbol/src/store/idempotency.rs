//! Shared lookup and insert helpers for the `idempotency_records` table.

use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use serde::Serialize;
use serde::de::DeserializeOwned;

use super::{
    IDEMPOTENCY_RETENTION_MILLIS, Idempotency, IdempotencyKind, StoreError, idempotency_key_hash,
    validate_idempotency_key,
};
use crate::schema::idempotency_records;

/// Returns the validated key of an optional idempotency request, or `None`
/// when the request carries no idempotency key.
pub(super) fn validated_key(idempotency: Option<&Idempotency>) -> Result<Option<&str>, StoreError> {
    let Some(idempotency) = idempotency else {
        return Ok(None);
    };
    validate_idempotency_key(&idempotency.key)?;
    Ok(Some(&idempotency.key))
}

/// Loads and decodes the record stored under `key`.
///
/// A stored record of a different `kind`, or (when `fingerprint` is given) with a
/// different fingerprint, is an `IdempotencyConflict`. The kind is checked
/// before the metadata is decoded.
pub(super) fn replay_record<T: DeserializeOwned>(
    tx: &mut SqliteConnection,
    key: &str,
    kind: IdempotencyKind,
    fingerprint: Option<&str>,
) -> Result<Option<T>, StoreError> {
    let record = idempotency_records::table
        .find(idempotency_key_hash(key))
        .select((
            idempotency_records::fingerprint,
            idempotency_records::operation_kind,
            idempotency_records::result_metadata,
        ))
        .first::<(String, i64, String)>(tx)
        .optional()?;
    let Some((stored_fingerprint, stored_kind, metadata)) = record else {
        return Ok(None);
    };
    if stored_kind != kind as i64 || fingerprint.is_some_and(|value| stored_fingerprint != value) {
        return Err(StoreError::IdempotencyConflict);
    }
    serde_json::from_str(&metadata)
        .map(Some)
        .map_err(StoreError::invalid_data)
}

/// Inserts the record for `key`, expiring after `IDEMPOTENCY_RETENTION_MILLIS`.
pub(super) fn store_record<T: Serialize>(
    tx: &mut SqliteConnection,
    key: &str,
    kind: IdempotencyKind,
    fingerprint: &str,
    value: &T,
    now: i64,
) -> Result<(), StoreError> {
    let metadata = serde_json::to_string(value).map_err(StoreError::invalid_data)?;
    diesel::insert_into(idempotency_records::table)
        .values((
            idempotency_records::key_hash.eq(idempotency_key_hash(key)),
            idempotency_records::fingerprint.eq(fingerprint),
            idempotency_records::operation_kind.eq(kind as i64),
            idempotency_records::result_metadata.eq(metadata),
            idempotency_records::expires.eq(now + IDEMPOTENCY_RETENTION_MILLIS),
        ))
        .execute(tx)?;
    Ok(())
}
