//! Streaming stored blobs: media types, ranges and caching headers.

use std::io::SeekFrom;

use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
use tokio_util::io::ReaderStream;

use crate::api_error::ApiError;
use crate::app::App;
use crate::hash::ContentHash;
use crate::headers::{insert_expiry_headers, insert_last_modified_header};
use crate::{html_charset, http_cache};

const STREAM_THRESHOLD: u64 = 1024 * 1024;

async fn send_blob(
    headers: &HeaderMap,
    content_type: &str,
    hash: ContentHash,
    app: &App,
) -> Result<Response, ApiError> {
    send_blob_file(
        headers,
        content_type,
        hash,
        http_cache::Policy::Revalidate,
        app,
    )
    .await
}

pub async fn send_expiring_blob(
    headers: &HeaderMap,
    name: &str,
    logical: &str,
    hash: ContentHash,
    app: &App,
) -> Result<Response, ApiError> {
    let report = app
        .run_store({
            let name = name.to_string();
            let logical = logical.to_string();
            move |store| store.expiry_report(&name, &logical)
        })
        .await?;
    let inferred = mime_guess::from_path(logical).first_or_octet_stream();
    let stored_media_type = app
        .run_store({
            let name = name.to_string();
            let logical = logical.to_string();
            move |store| store.allocated_media_type(&name, &logical)
        })
        .await?;
    let media_type = with_default_charset(
        stored_media_type
            .as_deref()
            .unwrap_or_else(|| inferred.essence_str()),
    );
    let media_type = with_html_charset(media_type, hash, app).await;
    let mut response = send_blob(headers, &media_type, hash, app).await?;
    let updated = app
        .run_store({
            let name = name.to_string();
            move |store| Ok(store.site_updated_at(&name))
        })
        .await
        .ok()
        .flatten();
    if let Some(updated) = updated {
        insert_last_modified_header(response.headers_mut(), updated);
    }
    insert_expiry_headers(response.headers_mut(), &report);
    Ok(response)
}

/// Labels text as UTF-8 unless it already names a charset.
///
/// `mime_guess` returns a bare essence such as `text/markdown`, and an unlabeled
/// text response leaves the browser to guess the encoding -- usually
/// windows-1252, which turns UTF-8 accents and emoji into mojibake. HTML and XML
/// are left alone here: both can declare their own encoding in-band
/// (`<meta charset>`, `<?xml encoding?>`), and an HTTP charset would silently
/// override an author's explicit declaration. XML without a declaration is
/// UTF-8 by definition; HTML is decided per file by [`with_html_charset`].
pub fn with_default_charset(media_type: &str) -> std::borrow::Cow<'_, str> {
    let mut parameters = media_type.split(';');
    let essence = parameters.next().unwrap_or_default().trim();
    let is_text = essence
        .get(..5)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("text/"));
    let self_declaring =
        essence.eq_ignore_ascii_case("text/html") || essence.eq_ignore_ascii_case("text/xml");
    let declares_charset = parameters.any(|parameter| {
        parameter
            .trim()
            .get(..8)
            .is_some_and(|name| name.eq_ignore_ascii_case("charset="))
    });
    if is_text && !self_declaring && !declares_charset {
        std::borrow::Cow::Owned(format!("{media_type}; charset=utf-8"))
    } else {
        std::borrow::Cow::Borrowed(media_type)
    }
}

/// Adds `charset=utf-8` to bare `text/html` when the page declares no encoding
/// of its own and is UTF-8 throughout. See [`html_charset`].
///
/// Anything that cannot be checked keeps the bare type, which is what it was
/// served as before.
async fn with_html_charset<'a>(
    media_type: std::borrow::Cow<'a, str>,
    hash: ContentHash,
    app: &App,
) -> std::borrow::Cow<'a, str> {
    let mut parameters = media_type.split(';');
    let bare_html = parameters
        .next()
        .is_some_and(|essence| essence.trim().eq_ignore_ascii_case("text/html"))
        && parameters.all(|parameter| {
            !parameter
                .trim()
                .get(..8)
                .is_some_and(|name| name.eq_ignore_ascii_case("charset="))
        });
    if !bare_html {
        return media_type;
    }
    let path = app.store.blob_path(hash);
    let utf8 = tokio::task::spawn_blocking(move || html_charset::labels_as_utf8(hash, &path))
        .await
        .is_ok_and(|verdict| verdict.unwrap_or(false));
    if utf8 {
        std::borrow::Cow::Owned(format!("{media_type}; charset=utf-8"))
    } else {
        media_type
    }
}

pub async fn add_target_expiry_headers(app: &App, name: &str, rel: &str, headers: &mut HeaderMap) {
    let report = app
        .run_store({
            let name = name.to_string();
            let rel = rel.to_string();
            move |store| store.expiry_report(&name, &rel)
        })
        .await;
    if let Ok(report) = report {
        insert_expiry_headers(headers, &report);
    }
}

pub async fn send_blob_file(
    headers: &HeaderMap,
    content_type: &str,
    hash: ContentHash,
    policy: http_cache::Policy,
    app: &App,
) -> Result<Response, ApiError> {
    let etag = format!("\"{}\"", hash.to_hex());
    if let Some(mut response) = http_cache::not_modified(headers, &etag, policy, None) {
        response
            .headers_mut()
            .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        return Ok(response);
    }

    let path = app.store.blob_path(hash);
    let size = tokio::fs::metadata(&path).await?.len();
    let Ok(range) = requested_range(headers, size, &etag) else {
        return Ok(range_not_satisfiable(size, &etag, policy));
    };

    if range.is_none() && size <= STREAM_THRESHOLD {
        let bytes = app.run_store(move |store| store.read_blob(hash)).await?;
        let mut representation = http_cache::Representation::new(bytes, "application/octet-stream");
        representation.content_type = HeaderValue::from_str(content_type).expect("valid MIME type");
        representation.policy = policy;
        representation.nosniff = true;
        representation.etag = Some(etag);
        let mut response = http_cache::respond(headers, representation);
        response
            .headers_mut()
            .insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
        response.headers_mut().insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&size.to_string()).expect("valid content length"),
        );
        return Ok(response);
    }

    let mut file = tokio::fs::File::open(path).await?;
    let (status, length, content_range) = match range {
        Some(range) => {
            file.seek(SeekFrom::Start(range.start)).await?;
            (
                StatusCode::PARTIAL_CONTENT,
                range.len(),
                Some(format!("bytes {}-{}/{}", range.start, range.end, size)),
            )
        }
        None => (StatusCode::OK, size, None),
    };
    let stream = ReaderStream::new(file.take(length));
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    let response_headers = response.headers_mut();
    response_headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(content_type).expect("valid MIME type"),
    );
    response_headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&length.to_string()).expect("valid content length"),
    );
    response_headers.insert(
        header::ETAG,
        HeaderValue::from_str(&etag).expect("valid ETag"),
    );
    response_headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(policy.value()),
    );
    response_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    response_headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if let Some(content_range) = content_range {
        response_headers.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&content_range).expect("valid content range"),
        );
    }
    Ok(response)
}

#[derive(Clone, Copy)]
pub struct ByteRange {
    pub start: u64,
    pub end: u64,
}

impl ByteRange {
    pub const fn len(self) -> u64 {
        self.end - self.start + 1
    }
}

pub fn requested_range(
    headers: &HeaderMap,
    size: u64,
    etag: &str,
) -> Result<Option<ByteRange>, ()> {
    let Some(value) = headers.get(header::RANGE) else {
        return Ok(None);
    };
    if headers
        .get(header::IF_RANGE)
        .is_some_and(|if_range| if_range.as_bytes() != etag.as_bytes())
    {
        return Ok(None);
    }
    let value = value.to_str().map_err(|_| ())?;
    let Some(range) = value.strip_prefix("bytes=") else {
        return Ok(None);
    };
    if range.contains(',') {
        return Ok(None);
    }
    let (start, end) = range.split_once('-').ok_or(())?;
    if size == 0 {
        return Err(());
    }
    if start.is_empty() {
        let suffix = end.parse::<u64>().map_err(|_| ())?;
        if suffix == 0 {
            return Err(());
        }
        let length = suffix.min(size);
        return Ok(Some(ByteRange {
            start: size - length,
            end: size - 1,
        }));
    }
    let start = start.parse::<u64>().map_err(|_| ())?;
    if start >= size {
        return Err(());
    }
    let end = if end.is_empty() {
        size - 1
    } else {
        end.parse::<u64>().map_err(|_| ())?.min(size - 1)
    };
    if end < start {
        return Err(());
    }
    Ok(Some(ByteRange { start, end }))
}

fn range_not_satisfiable(size: u64, etag: &str, policy: http_cache::Policy) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_RANGE,
        HeaderValue::from_str(&format!("bytes */{size}")).expect("valid content range"),
    );
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(etag).expect("valid ETag"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static(policy.value()),
    );
    response
}
