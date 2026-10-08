//! Parsing of request headers and writing of response headers.

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Response;

use crate::app::INTERNAL_CREATOR_HEADER;
use crate::expiry::ExpiryMode;
use crate::response::plain;
use crate::secrets::{ClaimToken, ManagementToken};
use crate::store::{CreationSecurity, CreatorIdentity, Idempotency, StoreError};
use crate::{expiry, name, sanitize, store};

pub fn optional_header<'a>(
    headers: &'a HeaderMap,
    name: &str,
) -> Result<Option<&'a str>, expiry::ExpiryError> {
    headers
        .get(name)
        .map(|value| {
            value
                .to_str()
                .map_err(|_| expiry::ExpiryError::InvalidDuration)
        })
        .transpose()
}

pub fn required_header<'a>(
    headers: &'a HeaderMap,
    name: &str,
    display_name: &'static str,
) -> Result<&'a str, expiry::ExpiryError> {
    optional_header(headers, name)?.ok_or(expiry::ExpiryError::MissingHeader(display_name))
}

pub fn destination_from(
    headers: &HeaderMap,
    required: bool,
) -> Result<Option<String>, &'static str> {
    let Some(value) = headers.get("destination") else {
        return if required {
            Err("error: Destination header is required")
        } else {
            Ok(None)
        };
    };
    let value = value
        .to_str()
        .map_err(|_| "error: invalid Destination header")?;
    let path = if value.starts_with('/') {
        value.to_string()
    } else {
        let uri = value
            .parse::<axum::http::Uri>()
            .map_err(|_| "error: invalid Destination header")?;
        uri.path().to_string()
    };
    let name = path.trim_matches('/');
    if name.contains('/') || name::parse_site_name(name).is_err() {
        return Err("error: Destination must identify one valid site");
    }
    Ok(Some(name.to_string()))
}

pub fn idempotency_from(headers: &HeaderMap) -> Option<Idempotency> {
    headers
        .get("idempotency-key")
        .and_then(|value| value.to_str().ok())
        .map(|key| Idempotency {
            key: key.to_string(),
        })
}

pub fn management_bearer(headers: &HeaderMap) -> Result<Option<ManagementToken>, StoreError> {
    let Some(value) = headers.get(header::AUTHORIZATION) else {
        return Ok(None);
    };
    let value = value.to_str().map_err(|_| StoreError::Unauthorized)?;
    let encoded = value
        .strip_prefix("Bearer ")
        .ok_or(StoreError::Unauthorized)?;
    ManagementToken::parse(encoded)
        .map(Some)
        .map_err(|_| StoreError::Unauthorized)
}

pub fn creator_claim(headers: &HeaderMap) -> Result<Option<ClaimToken>, &'static str> {
    headers
        .get("creator-claim")
        .map(|value| {
            value
                .to_str()
                .map_err(|_| "error: invalid Creator-Claim header")
                .and_then(|value| {
                    ClaimToken::parse(value).map_err(|_| "error: invalid Creator-Claim header")
                })
        })
        .transpose()
}

pub fn creator_identity(headers: &HeaderMap) -> Option<CreatorIdentity> {
    headers
        .get(INTERNAL_CREATOR_HEADER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .and_then(|value| value.split_once(':'))
        .and_then(|(kind, principal)| match kind {
            "proxy" => Some(CreatorIdentity::trusted_proxy(principal)),
            "mtls" => Some(CreatorIdentity::mtls(principal)),
            "tailscale" => Some(CreatorIdentity::tailscale(principal)),
            _ => None,
        })
}

pub struct CreationRequest {
    pub security: CreationSecurity,
    management_token: Option<ManagementToken>,
    claim_token: Option<ClaimToken>,
    managed: bool,
}

#[expect(clippy::result_large_err)] // the large variant is Response, not StoreError
pub fn creation_request(headers: &HeaderMap) -> Result<CreationRequest, Response> {
    let managed = match headers
        .get("management-action")
        .map(|value| value.to_str())
        .transpose()
    {
        Ok(None) => false,
        Ok(Some("claim")) => true,
        Ok(Some(_)) => {
            return Err(plain(
                StatusCode::BAD_REQUEST,
                "error: Management-Action must be claim on creation",
            ));
        }
        Err(_) => {
            return Err(plain(
                StatusCode::BAD_REQUEST,
                "error: invalid Management-Action header",
            ));
        }
    };
    let creator = creator_identity(headers);
    let supplied_claim =
        creator_claim(headers).map_err(|message| plain(StatusCode::BAD_REQUEST, message))?;
    let claim_token = if creator.is_none() && supplied_claim.is_none() {
        Some(
            ClaimToken::generate()
                .map_err(|_| plain(StatusCode::INTERNAL_SERVER_ERROR, "error: random source"))?,
        )
    } else {
        None
    };
    let claim_hash = supplied_claim
        .as_ref()
        .or(claim_token.as_ref())
        .map(ClaimToken::hash);
    let management_token = if managed {
        Some(
            ManagementToken::generate()
                .map_err(|_| plain(StatusCode::INTERNAL_SERVER_ERROR, "error: random source"))?,
        )
    } else {
        None
    };
    Ok(CreationRequest {
        security: CreationSecurity {
            creator,
            claim_hash,
            management_hash: management_token.as_ref().map(ManagementToken::hash),
        },
        management_token,
        claim_token,
        managed,
    })
}

pub fn insert_creation_headers(response: &mut Response, creation: CreationRequest, created: bool) {
    if !created {
        return;
    }
    let headers = response.headers_mut();
    if let Some(token) = creation.management_token {
        headers.insert(
            "management-token",
            HeaderValue::from_str(&token.encode()).expect("token is a valid header"),
        );
    }
    if let Some(token) = creation.claim_token {
        headers.insert(
            "creator-claim",
            HeaderValue::from_str(&token.encode()).expect("claim is a valid header"),
        );
    }
    if creation.managed || headers.contains_key("creator-claim") {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
}

pub fn insert_sanitized_headers(headers: &mut HeaderMap, counts: sanitize::TokenCounts) {
    if counts.management > 0 {
        headers.insert(
            "sanitized-management-tokens",
            HeaderValue::from_str(&counts.management.to_string()).expect("valid count"),
        );
    }
    if counts.claim > 0 {
        headers.insert(
            "sanitized-creator-claims",
            HeaderValue::from_str(&counts.claim.to_string()).expect("valid count"),
        );
    }
}

pub fn wants_unpack(headers: &HeaderMap) -> bool {
    truthy_flag(headers, "unpack")
}

pub fn wants_replace(headers: &HeaderMap) -> bool {
    truthy_flag(headers, "replace")
}

fn truthy_flag(headers: &HeaderMap, name: &'static str) -> bool {
    headers.get(name).is_some_and(|v| {
        v.to_str().is_ok_and(|v| {
            let v = v.trim();
            v.is_empty()
                || v.eq_ignore_ascii_case("1")
                || v.eq_ignore_ascii_case("true")
                || v.eq_ignore_ascii_case("yes")
        })
    })
}

pub fn if_match_from(headers: &HeaderMap) -> Result<Option<String>, &'static str> {
    let Some(value) = headers.get(header::IF_MATCH) else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .map_err(|_| "error: invalid If-Match header")?
        .trim();
    let value = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
        .unwrap_or(value);
    if value.contains(',')
        || value == "*"
        || !value.starts_with("blake3:")
        || value.len() != "blake3:".len() + 64
    {
        return Err("error: If-Match must contain one site tree ETag");
    }
    Ok(Some(value.to_string()))
}

pub fn filename_from(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::CONTENT_DISPOSITION)
        .and_then(|v| v.to_str().ok())
        .and_then(parse_filename)
}

fn parse_filename(cd: &str) -> Option<String> {
    for part in cd.split(';') {
        let part = part.trim();
        let Some(rest) = part.strip_prefix("filename=") else {
            continue;
        };
        let rest = rest.trim();
        let rest = rest
            .strip_prefix('"')
            .and_then(|s| s.strip_suffix('"'))
            .unwrap_or(rest);
        if !rest.is_empty() {
            return Some(rest.to_string());
        }
    }
    None
}

pub fn insert_undo_headers(headers: &mut HeaderMap, undo: &store::UndoInfo) {
    headers.insert(
        "undo-token",
        HeaderValue::from_str(&undo.token).expect("valid undo token"),
    );
    headers.insert(
        "undo-expires",
        HeaderValue::from_str(&undo.expires_at).expect("valid undo expiry"),
    );
}

/// Formats a timestamp as an HTTP date, e.g. `Thu, 01 Jan 1970 00:00:00 GMT`.
fn http_date(timestamp: time::OffsetDateTime) -> String {
    timestamp
        .to_offset(time::UtcOffset::UTC)
        .format(
            &time::format_description::parse_borrowed::<2>(
                "[weekday repr:short], [day padding:zero] [month repr:short] [year] [hour]:[minute]:[second] GMT",
            )
            .expect("valid HTTP date format"),
        )
        .expect("HTTP date is representable")
}

pub fn insert_last_modified_header(headers: &mut HeaderMap, updated_millis: i64) {
    let updated =
        time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(updated_millis) * 1_000_000)
            .expect("site timestamp is representable");
    headers.insert(
        header::LAST_MODIFIED,
        HeaderValue::from_str(&http_date(updated)).expect("valid Last-Modified header"),
    );
}

pub fn insert_expiry_headers(headers: &mut HeaderMap, report: &expiry::ExpiryReport) {
    let Some(expires_at) = &report.effective_expires_at else {
        return;
    };
    let timestamp =
        expiry::parse_rfc3339_timestamp(expires_at).expect("stored expiry timestamp is valid");
    headers.insert(
        header::EXPIRES,
        HeaderValue::from_str(&http_date(timestamp)).expect("valid Expires header"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    if let Some(own) = &report.own_policy {
        headers.insert(
            "expiry-mode",
            HeaderValue::from_static(match own.mode {
                ExpiryMode::Relative => "relative",
                ExpiryMode::Absolute => "absolute",
                ExpiryMode::Decay => "decay",
            }),
        );
    }
}
