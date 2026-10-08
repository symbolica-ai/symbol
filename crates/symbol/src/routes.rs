//! The route table and the layers around it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request};
use axum::middleware::{self, Next};
use axum::response::Response;
use axum::routing::{MethodRouter, get};
use symbol_contract as contract;
use tower_http::trace::{DefaultOnRequest, DefaultOnResponse, TraceLayer};

use crate::app::{App, RequestIdentityConfig, attach_api_identity, resolve_creator};
use crate::docs::{
    api_d_ts, api_d_ts_hash, api_global_js, api_global_js_hash, api_index, api_javascript, api_js,
    api_js_hash, api_manual_method_not_allowed, api_manual_not_found, api_markdown,
    api_markdown_raw, api_protocol, api_py, api_py_hash, api_python, api_redirect, api_shell,
    api_ts, api_ts_hash, api_version, docs, docs_hash, install_sh, install_sh_hash, stats,
    symbol_sh, symbol_sh_hash,
};
use crate::lifecycle::{
    content_method, control_namespace_method, expiry_site_report, files_content_method,
    lifecycle_method, list_sites, site_root_method, undo_stack,
};
use crate::mutation_http;
use crate::publish::{
    delete_file, delete_site, put_file, put_site, put_site_unnamed, redirect_site,
};
use crate::serve::{
    browse_path, browse_root, render_asset, serve_immutable_blob, serve_index, serve_path,
};

static NEXT_MUTATION_SIGNAL_ID: AtomicU64 = AtomicU64::new(1);

pub static CUSTOM_MUTATION_METHODS: LazyLock<[Method; 7]> = LazyLock::new(|| {
    [
        contract::METHOD_ALIAS,
        contract::METHOD_COPY,
        contract::METHOD_REPLACE,
        contract::METHOD_MOVE,
        contract::METHOD_UNDO,
        contract::METHOD_EXPIRE,
        contract::METHOD_MANAGE,
    ]
    .map(|method| Method::from_bytes(method.as_bytes()).expect("contract method is valid"))
});

pub fn router(app: App) -> Router {
    let identity_config = RequestIdentityConfig {
        provider: app.identity_provider.clone(),
        audit_trusted_proxy: Arc::clone(&app.audit_trusted_proxy),
    };
    Router::new()
        .route(contract::ROOT, get(docs).put(put_site_unnamed))
        .route(contract::HASH, get(docs_hash))
        .route(contract::STATS, get(stats))
        .route(contract::STATS_SLASH, get(stats))
        .route(contract::INSTALL, get(install_sh))
        .route(contract::INSTALL_HASH, get(install_sh_hash))
        .route(contract::CLIENT, get(symbol_sh))
        .route(contract::CLIENT_HASH, get(symbol_sh_hash))
        .merge(api_router())
        .route(contract::FILES, get(list_sites))
        .route(contract::FILES_SLASH, get(list_sites))
        .route(
            contract::SITE_FILES,
            control_namespace_methods(get(browse_root)),
        )
        .route(
            contract::SITE_FILES_SLASH,
            control_namespace_methods(get(browse_root)),
        )
        .route(
            contract::SITE_FILES_PATH,
            get(browse_path)
                .post(mutation_http::allocate_files_path)
                .patch(mutation_http::splice_files_path)
                .fallback(files_content_method),
        )
        .route(
            contract::SITE_UNDO,
            control_namespace_methods(get(undo_stack)),
        )
        .route(
            contract::SITE_UNDO_SLASH,
            control_namespace_methods(get(undo_stack)),
        )
        .route(
            contract::SITE_EXPIRES,
            control_namespace_methods(get(expiry_site_report)),
        )
        .route(
            contract::SITE_EXPIRES_SLASH,
            control_namespace_methods(get(expiry_site_report)),
        )
        .route(
            contract::SITE_ROOT,
            get(serve_index)
                .post(mutation_http::allocate_root)
                .put(put_site)
                .delete(delete_site)
                .fallback(site_root_method),
        )
        .route(contract::IMMUTABLE_BLOB, get(serve_immutable_blob))
        .route(contract::RENDER_ASSET, get(render_asset))
        .route(
            contract::SITE_PATH,
            get(serve_path)
                .post(mutation_http::allocate_path)
                .patch(mutation_http::splice_file)
                .put(put_file)
                .delete(delete_file)
                .fallback(content_method),
        )
        .route(
            contract::SITE,
            get(redirect_site)
                .put(put_site)
                .delete(delete_site)
                .fallback(lifecycle_method),
        )
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(make_http_span)
                .on_request(DefaultOnRequest::new().level(tracing::Level::INFO))
                .on_response(DefaultOnResponse::new().level(tracing::Level::INFO)),
        )
        .layer(middleware::from_fn_with_state(
            identity_config,
            resolve_creator,
        ))
        .layer(middleware::from_fn(log_mutation_activity))
        .layer(middleware::from_fn(attach_api_identity))
        .with_state(app)
}

fn api_router() -> Router<App> {
    // Each generated client is served at its contract path and at a short
    // `/api.*` alias, with a `/HASH` sibling for both.
    let assets = [
        (
            contract::API_TS,
            contract::API_TS_HASH,
            "/api.ts",
            get(api_ts),
            get(api_ts_hash),
        ),
        (
            contract::API_JS,
            contract::API_JS_HASH,
            "/api.js",
            get(api_js),
            get(api_js_hash),
        ),
        (
            contract::API_GLOBAL_JS,
            contract::API_GLOBAL_JS_HASH,
            "/api.global.js",
            get(api_global_js),
            get(api_global_js_hash),
        ),
        (
            contract::API_D_TS,
            contract::API_D_TS_HASH,
            "/api.d.ts",
            get(api_d_ts),
            get(api_d_ts_hash),
        ),
        (
            contract::API_PY,
            contract::API_PY_HASH,
            "/api.py",
            get(api_py),
            get(api_py_hash),
        ),
    ];
    let router = assets.into_iter().fold(
        Router::new(),
        |router, (path, hash_path, alias, asset, hash)| {
            let alias_hash = format!("{alias}/HASH");
            router
                .route(path, asset.clone())
                .route(hash_path, hash.clone())
                .route(alias, asset)
                .route(&alias_hash, hash)
        },
    );
    let manuals = [
        (contract::API, get(api_redirect)),
        (contract::API_INDEX, get(api_index)),
        (contract::API_JS_MANUAL, get(api_javascript)),
        (contract::API_TS_MANUAL, get(api_javascript)),
        (contract::API_PY_MANUAL, get(api_python)),
        (contract::API_PYTHON_MANUAL, get(api_python)),
        (contract::API_SH_MANUAL, get(api_shell)),
        (contract::API_CURL_MANUAL, get(api_protocol)),
        (contract::API_HTTP_MANUAL, get(api_protocol)),
        (contract::API_REST_MANUAL, get(api_protocol)),
        (contract::API_PROTOCOL_MANUAL, get(api_protocol)),
        (contract::API_MARKDOWN_MANUAL, get(api_markdown)),
        (contract::API_MD_MANUAL, get(api_markdown)),
        (contract::API_MARKDOWN_RAW, get(api_markdown_raw)),
        (contract::API_VERSION, get(api_version)),
        (contract::API_PATH, get(api_manual_not_found)),
    ];
    manuals.into_iter().fold(router, |router, (path, methods)| {
        router.route(path, api_manual_methods(methods))
    })
}

fn control_namespace_methods(methods: MethodRouter<App>) -> MethodRouter<App> {
    methods
        .post(mutation_http::reject_control_allocation)
        .put(mutation_http::reject_control_allocation)
        .patch(mutation_http::reject_control_allocation)
        .fallback(control_namespace_method)
}

fn api_manual_methods(methods: MethodRouter<App>) -> MethodRouter<App> {
    methods.fallback(api_manual_method_not_allowed)
}

pub fn make_http_span(request: &Request<Body>) -> tracing::Span {
    tracing::info_span!(
        "http_request",
        method = %request.method(),
        uri = %request.uri()
    )
}

pub async fn log_mutation_activity(request: Request<Body>, next: Next) -> Response {
    if !is_mutation_method(request.method()) {
        return next.run(request).await;
    }
    let mutation_id = NEXT_MUTATION_SIGNAL_ID.fetch_add(1, Ordering::Relaxed);
    let method = request.method().clone();
    tracing::info!(
        target: "symbol::mutation",
        mutation_id,
        method = %method,
        "symbol_mutation_start"
    );
    let response = next.run(request).await;
    tracing::info!(
        target: "symbol::mutation",
        mutation_id,
        method = %method,
        status = response.status().as_u16(),
        "symbol_mutation_finish"
    );
    response
}

pub fn log_mutation_signals_ready() {
    tracing::info!(
        target: "symbol::mutation",
        "symbol_mutation_signals_ready"
    );
}

pub fn is_mutation_method(method: &Method) -> bool {
    method == Method::PUT
        || method == Method::DELETE
        || method == Method::POST
        || method == Method::PATCH
        || CUSTOM_MUTATION_METHODS
            .iter()
            .any(|candidate| method == candidate)
}
