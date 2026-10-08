//! Site lifecycle requests: copy, move, undo, expire, manage, and listings.

use std::net::IpAddr;

use axum::Json;
use axum::body::Body;
use axum::extract::{Extension, OriginalUri, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use symbol_contract as contract;

use crate::api_error::ApiError;
use crate::app::{App, AuditIp};
use crate::expiry::{DecayPolicy, ExpiryPolicy};
use crate::headers::{
    creation_request, creator_claim, creator_identity, destination_from, idempotency_from,
    insert_expiry_headers, insert_undo_headers, management_bearer, optional_header,
    required_header,
};
use crate::publish::authorize;
use crate::response::{creation_response, json_no_cache, mutation_response, plain};
use crate::store::{ManagementRequest, StoreError};
use crate::{browse, expiry, mutation_http, store};

#[derive(Clone, Copy)]
enum ManagementAction {
    Claim,
    Status,
    Rotate,
    Release,
}

pub async fn list_sites(
    State(app): State<App>,
    OriginalUri(uri): OriginalUri,
    Query(query): Query<browse::ListingQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let names = app.run_store(|store| store.list_sites()).await?;
    Ok(browse::sites(&headers, &uri, &query, names))
}

pub async fn undo_stack(
    State(app): State<App>,
    Path(name): Path<String>,
) -> Result<Response, ApiError> {
    let stack = app.run_store(move |store| store.undo_stack(&name)).await?;
    Ok(json_no_cache(stack))
}

pub async fn expiry_site_report(
    State(app): State<App>,
    Path(name): Path<String>,
) -> Result<Response, ApiError> {
    let report = app
        .run_store(move |store| store.expiry_site_report(&name))
        .await?;
    Ok(json_no_cache(contract::ExpirySiteReport::from(&report)))
}

pub async fn expiry_path_report(
    app: &App,
    name: String,
    path: String,
) -> Result<Response, ApiError> {
    let report = app
        .run_store(move |store| store.expiry_report(&name, &path))
        .await?;
    Ok(json_no_cache(contract::ExpiryReport::from(&report)))
}

pub async fn lifecycle_method(
    State(app): State<App>,
    Path(name): Path<String>,
    Extension(AuditIp(peer)): Extension<AuditIp>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    lifecycle_dispatch(&app, &name, peer, &method, &headers).await
}

pub async fn site_root_method(
    State(app): State<App>,
    Path(name): Path<String>,
    Extension(AuditIp(peer)): Extension<AuditIp>,
    method: Method,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    if method.as_str() == contract::METHOD_ALIAS {
        return Ok(mutation_http::alias_batch(&app, &name, &headers, body).await);
    }
    lifecycle_dispatch(&app, &name, peer, &method, &headers).await
}

async fn lifecycle_dispatch(
    app: &App,
    name: &str,
    peer: Option<IpAddr>,
    method: &Method,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    match method.as_str() {
        contract::METHOD_UNDO => undo_site(app, name, headers).await,
        contract::METHOD_COPY => copy_site(app, name, headers).await,
        contract::METHOD_MOVE => move_site(app, name, headers).await,
        contract::METHOD_EXPIRE => expire_target(app, name, "", headers).await,
        contract::METHOD_MANAGE => manage_site(app, name, headers, peer).await,
        _ => Ok(plain(
            StatusCode::METHOD_NOT_ALLOWED,
            "error: method not allowed",
        )),
    }
}

async fn manage_site(
    app: &App,
    name: &str,
    headers: &HeaderMap,
    audit_ip: Option<IpAddr>,
) -> Result<Response, ApiError> {
    let action = match headers
        .get("management-action")
        .and_then(|value| value.to_str().ok())
    {
        Some("claim") => ManagementAction::Claim,
        Some("status") => ManagementAction::Status,
        Some("rotate") => ManagementAction::Rotate,
        Some("release") => ManagementAction::Release,
        Some(_) => return Err("error: invalid Management-Action header".into()),
        None => return Err("error: Management-Action header is required".into()),
    };
    let bearer = management_bearer(headers)?;
    let claim = creator_claim(headers)?;
    let creator = creator_identity(headers);
    let idempotency = idempotency_from(headers);
    let audit_ip = audit_ip.map(|ip| ip.to_string());
    let name_owned = name.to_string();
    let mutation = app
        .run_store(move |store| match action {
            ManagementAction::Claim => store.claim_management(
                &name_owned,
                creator,
                claim.as_ref(),
                ManagementRequest {
                    idempotency: idempotency.as_ref(),
                    audit_ip: audit_ip.as_deref(),
                },
            ),
            ManagementAction::Rotate => store.rotate_management(
                &name_owned,
                bearer.as_ref(),
                creator,
                claim.as_ref(),
                ManagementRequest {
                    idempotency: idempotency.as_ref(),
                    audit_ip: audit_ip.as_deref(),
                },
            ),
            ManagementAction::Status => {
                store
                    .management_status(&name_owned)
                    .map(|status| store::ManagementMutation {
                        status,
                        token: None,
                        replayed: false,
                    })
            }
            ManagementAction::Release => store
                .release_management(&name_owned, bearer.as_ref(), audit_ip.as_deref())
                .map(|status| store::ManagementMutation {
                    status,
                    token: None,
                    replayed: false,
                }),
        })
        .await?;
    let mut response = Json(mutation.status).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Some(token) = mutation.token {
        response.headers_mut().insert(
            "management-token",
            HeaderValue::from_str(&token.encode()).expect("token is a valid header"),
        );
    }
    if mutation.replayed {
        response
            .headers_mut()
            .insert("idempotency-replayed", HeaderValue::from_static("true"));
    }
    Ok(response)
}

pub async fn content_method(
    State(app): State<App>,
    Path((name, path)): Path<(String, String)>,
    method: Method,
    headers: HeaderMap,
    body: Body,
) -> Response {
    match method.as_str() {
        contract::METHOD_EXPIRE => expire_target(&app, &name, path.trim_end_matches('/'), &headers)
            .await
            .into_response(),
        contract::METHOD_ALIAS => {
            mutation_http::alias_file(&app, &name, path.trim_end_matches('/'), &headers).await
        }
        contract::METHOD_REPLACE => {
            mutation_http::replace_file(&app, &name, path.trim_end_matches('/'), &headers, body)
                .await
        }
        _ => plain(StatusCode::METHOD_NOT_ALLOWED, "error: method not allowed"),
    }
}

pub async fn files_content_method(
    State(app): State<App>,
    Path((name, _path)): Path<(String, String)>,
    method: Method,
    headers: HeaderMap,
    _body: Body,
) -> Response {
    match method.as_str() {
        "PUT"
        | "DELETE"
        | contract::METHOD_EXPIRE
        | contract::METHOD_ALIAS
        | contract::METHOD_REPLACE => {
            mutation_http::reject_control_mutation(&app, &name, &headers).await
        }
        _ => plain(StatusCode::METHOD_NOT_ALLOWED, "error: method not allowed"),
    }
}

pub async fn control_namespace_method(
    State(app): State<App>,
    Path(name): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Response {
    match method.as_str() {
        "DELETE" | contract::METHOD_EXPIRE | contract::METHOD_ALIAS | contract::METHOD_REPLACE => {
            mutation_http::reject_control_mutation(&app, &name, &headers).await
        }
        _ => plain(StatusCode::METHOD_NOT_ALLOWED, "error: method not allowed"),
    }
}

async fn expire_target(
    app: &App,
    name: &str,
    path: &str,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    let authorization = authorize(app, name, headers).await?;
    store::validate_mutation_target(path)?;
    let policy =
        expiry_policy_from(headers, app.store.expiry_defaults()).map_err(StoreError::Expiry)?;
    let name = name.to_string();
    let path = path.to_string();
    let mutation = app
        .run_store(move |store| match policy {
            ExpiryRequest::Default => {
                store.set_default_expiry_secured(&name, &path, authorization.as_ref())
            }
            ExpiryRequest::Never => {
                store.set_expiry_secured(&name, &path, None, authorization.as_ref())
            }
            ExpiryRequest::Policy(policy) => {
                store.set_expiry_secured(&name, &path, Some(policy), authorization.as_ref())
            }
        })
        .await?;
    let mut response = Json(contract::ExpiryReport::from(&mutation.report)).into_response();
    let response_headers = response.headers_mut();
    response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    if let Some(undo) = &mutation.undo {
        insert_undo_headers(response_headers, undo);
    }
    insert_expiry_headers(response_headers, &mutation.report);
    Ok(response)
}

#[derive(Clone, Copy)]
pub enum ExpiryRequest {
    Default,
    Never,
    Policy(ExpiryPolicy),
}

pub fn expiry_policy_from(
    headers: &HeaderMap,
    defaults: DecayPolicy,
) -> Result<ExpiryRequest, expiry::ExpiryError> {
    let Some(mode) = optional_header(headers, "expiry-mode")? else {
        if has_expiry_parameters(headers) {
            return Err(expiry::ExpiryError::InvalidModeHeader);
        }
        return Ok(ExpiryRequest::Default);
    };
    match mode {
        "never" if !has_expiry_parameters(headers) => Ok(ExpiryRequest::Never),
        "relative" => {
            if headers.contains_key("expiry-at") || has_decay_parameters(headers) {
                return Err(expiry::ExpiryError::InvalidModeHeader);
            }
            let duration = required_header(headers, "expiry-in", "Expiry-In")?;
            Ok(ExpiryRequest::Policy(ExpiryPolicy::Relative {
                duration_seconds: expiry::parse_duration_seconds(duration)?,
            }))
        }
        "absolute" => {
            if headers.contains_key("expiry-in") || has_decay_parameters(headers) {
                return Err(expiry::ExpiryError::InvalidModeHeader);
            }
            let timestamp = required_header(headers, "expiry-at", "Expiry-At")?;
            Ok(ExpiryRequest::Policy(ExpiryPolicy::Absolute {
                deadline_unix_seconds: expiry::parse_rfc3339_timestamp(timestamp)?.unix_timestamp(),
            }))
        }
        "decay" => {
            if headers.contains_key("expiry-in") || headers.contains_key("expiry-at") {
                return Err(expiry::ExpiryError::InvalidModeHeader);
            }
            let min_age_seconds = optional_header(headers, "expiry-min-age")?
                .map(expiry::parse_duration_seconds)
                .transpose()?
                .unwrap_or(defaults.min_age_seconds);
            let max_age_seconds = optional_header(headers, "expiry-max-age")?
                .map(expiry::parse_duration_seconds)
                .transpose()?
                .unwrap_or(defaults.max_age_seconds);
            let max_size_bytes = optional_header(headers, "expiry-max-size")?
                .map(expiry::parse_size_bytes)
                .transpose()?
                .unwrap_or(defaults.max_size_bytes);
            let power = optional_header(headers, "expiry-power")?
                .map(|value| {
                    value
                        .parse::<f64>()
                        .map_err(|_| expiry::ExpiryError::InvalidPower)
                })
                .transpose()?
                .unwrap_or(defaults.power);
            Ok(ExpiryRequest::Policy(ExpiryPolicy::Decay(
                DecayPolicy {
                    min_age_seconds,
                    max_age_seconds,
                    max_size_bytes,
                    power,
                }
                .validate()?,
            )))
        }
        _ => Err(expiry::ExpiryError::InvalidModeHeader),
    }
}

fn has_decay_parameters(headers: &HeaderMap) -> bool {
    [
        "expiry-min-age",
        "expiry-max-age",
        "expiry-max-size",
        "expiry-power",
    ]
    .iter()
    .any(|name| headers.contains_key(*name))
}

pub fn has_expiry_parameters(headers: &HeaderMap) -> bool {
    headers.contains_key("expiry-in")
        || headers.contains_key("expiry-at")
        || has_decay_parameters(headers)
}

async fn undo_site(app: &App, name: &str, headers: &HeaderMap) -> Result<Response, ApiError> {
    let authorization = management_bearer(headers)?;
    let guard = headers
        .get("undo-token")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let restored = app
        .run_store({
            let name = name.to_string();
            move |store| store.undo_secured(&name, guard.as_deref(), authorization.as_ref())
        })
        .await?;
    Ok(plain(
        StatusCode::OK,
        format!("restored {name} to {}", restored.restored_at),
    ))
}

async fn copy_site(app: &App, source: &str, headers: &HeaderMap) -> Result<Response, ApiError> {
    let creation = creation_request(headers)?;
    let destination = destination_from(headers, false)?;
    let idempotency = if destination.is_none() {
        idempotency_from(headers)
    } else {
        None
    };
    let source = source.to_string();
    let (name, mutation) = app
        .run_store(move |store| {
            store.copy_site_secured(
                &source,
                destination.as_deref(),
                idempotency.as_ref(),
                creation.security,
            )
        })
        .await?;
    let url = format!("{}/{name}/", app.public_url);
    Ok(creation_response(
        StatusCode::CREATED,
        &url,
        format!("ok {name} {url} ({} files)", mutation.files),
        &mutation,
        creation,
        None,
    ))
}

async fn move_site(app: &App, source: &str, headers: &HeaderMap) -> Result<Response, ApiError> {
    let authorization = management_bearer(headers)?;
    let Some(destination) = destination_from(headers, true)? else {
        unreachable!("required destination was checked");
    };
    let source = source.to_string();
    let old_name = source.clone();
    let (name, mutation) = app
        .run_store(move |store| {
            store.move_site_secured(&source, &destination, authorization.as_ref())
        })
        .await?;
    let url = format!("{}/{name}/", app.public_url);
    Ok(mutation_response(
        StatusCode::OK,
        &url,
        format!("moved {}/{old_name}/ -> {url}", app.public_url),
        &mutation,
    ))
}
