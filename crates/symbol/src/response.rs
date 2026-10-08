//! Response constructors, and how `StoreError` becomes a response.

use axum::Json;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::headers::{
    CreationRequest, insert_creation_headers, insert_sanitized_headers, insert_undo_headers,
};
use crate::store::StoreError;
use crate::{sanitize, store, upload};

/// A JSON body that caches must revalidate.
pub fn json_no_cache(body: impl serde::Serialize) -> Response {
    let mut response = Json(body).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

/// `201 Created` when the request made something new, otherwise `200 OK`.
pub const fn created_or_ok(mutation: &store::MutationResult) -> StatusCode {
    if mutation.created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    }
}

/// The response to a request that may have created something: the mutation
/// headers, any sanitized-token counts, then the creation secrets.
pub fn creation_response(
    status: StatusCode,
    location: &str,
    body: String,
    mutation: &store::MutationResult,
    creation: CreationRequest,
    sanitized: Option<sanitize::TokenCounts>,
) -> Response {
    let mut response = mutation_response(status, location, body, mutation);
    if let Some(counts) = sanitized {
        insert_sanitized_headers(response.headers_mut(), counts);
    }
    insert_creation_headers(
        &mut response,
        creation,
        mutation.created && !mutation.replayed,
    );
    response
}

pub fn mutation_response(
    status: StatusCode,
    location: &str,
    body: String,
    mutation: &store::MutationResult,
) -> Response {
    let mut response = plain(status, body);
    let headers = response.headers_mut();
    headers.insert(
        header::LOCATION,
        HeaderValue::from_str(location).expect("valid public URL"),
    );
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(&format!("\"{}\"", mutation.tree_hash)).expect("valid ETag"),
    );
    headers.insert(
        "content-revision",
        HeaderValue::from_str(&mutation.revision.to_string()).expect("valid revision"),
    );
    if mutation.replayed {
        headers.insert("idempotency-replayed", HeaderValue::from_static("true"));
    }
    if let Some(undo) = &mutation.undo {
        insert_undo_headers(headers, undo);
    }
    response
}

pub fn plain(status: StatusCode, body: impl AsRef<str>) -> Response {
    let body = body.as_ref();
    let mut out = body.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    (
        status,
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        out,
    )
        .into_response()
}

impl IntoResponse for StoreError {
    fn into_response(self) -> Response {
        if matches!(self, Self::Unauthorized) {
            let mut response = plain(StatusCode::UNAUTHORIZED, self.to_string());
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                HeaderValue::from_static("Bearer realm=\"symbol\""),
            );
            response
                .headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            return response;
        }
        if let Self::PreconditionFailed {
            revision,
            tree_hash,
        } = &self
        {
            let mut response = plain(StatusCode::PRECONDITION_FAILED, self.to_string());
            let headers = response.headers_mut();
            headers.insert(
                header::ETAG,
                HeaderValue::from_str(&format!("\"{}\"", tree_hash.to_wire()))
                    .expect("valid site ETag"),
            );
            headers.insert(
                "content-revision",
                HeaderValue::from_str(&revision.to_string()).expect("valid revision"),
            );
            return response;
        }
        if let Self::StaleContentHash(current_hash) = &self {
            let mut response = plain(StatusCode::PRECONDITION_FAILED, self.to_string());
            response.headers_mut().insert(
                header::ETAG,
                HeaderValue::from_str(&format!("\"{}\"", current_hash.to_wire()))
                    .expect("valid content ETag"),
            );
            return response;
        }
        let status = match &self {
            Self::NotFound | Self::InvalidPendingAllocation => StatusCode::NOT_FOUND,
            Self::StaleUndo(_)
            | Self::DestinationConflict
            | Self::AliasConflict
            | Self::AliasWrite
            | Self::AliasCycle
            | Self::AliasHopLimit
            | Self::IdempotencyConflict
            | Self::AlreadyManaged => StatusCode::CONFLICT,
            Self::Forbidden => StatusCode::FORBIDDEN,
            Self::Upload(
                upload::UploadError::ArchiveTooLarge
                | upload::UploadError::FileTooLarge
                | upload::UploadError::TooLarge
                | upload::UploadError::TooManyFiles,
            )
            | Self::SpliceResultTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::SpliceRange => StatusCode::RANGE_NOT_SATISFIABLE,
            Self::Io(_)
            | Self::Sqlite(_)
            | Self::Connection(_)
            | Self::Migration(_)
            | Self::Random(_)
            | Self::UnsupportedUndoKind(_) => StatusCode::INTERNAL_SERVER_ERROR,
            _ => StatusCode::BAD_REQUEST,
        };
        plain(status, self.to_string())
    }
}
