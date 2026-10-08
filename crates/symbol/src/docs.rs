//! The built-in pages, generated clients and install scripts.

use std::sync::LazyLock;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};

use crate::api_error::ApiError;
use crate::app::App;
use crate::response::plain;
use crate::{http_cache, page};

const INSTALL_SH: &str = static_asset!("install.sh");
const SYMBOL_SH: &str = static_asset!("symbol.sh");
const API_TS: &str = generated_asset!("symbol.ts");
const API_JS: &str = generated_asset!("symbol.js");
const API_GLOBAL_JS: &str = generated_asset!("symbol.global.js");
const API_D_TS: &str = generated_asset!("symbol.d.ts");
const API_PY: &str = generated_asset!("symbol.py");

pub const API_VERSION: &str = env!("SYMBOL_API_VERSION");
pub const API_REVISION: &str = env!("SYMBOL_API_REVISION");
pub const API_SOURCE_HASH: &str = env!("SYMBOL_API_SOURCE_HASH");
pub const API_COMMIT: &str = env!("SYMBOL_API_COMMIT");
pub const API_DIRTY: &str = env!("SYMBOL_API_DIRTY");

static API_VERSION_DOCUMENT: LazyLock<String> = LazyLock::new(|| {
    serde_json::json!({
        "api_version": API_VERSION,
        "absolute_revision": API_REVISION.parse::<u64>().expect("generated API revision"),
        "source_hash": API_SOURCE_HASH,
        "commit": API_COMMIT,
        "dirty": API_DIRTY == "true",
    })
    .to_string()
});

fn script_body(template: &str, public_url: &str) -> String {
    template
        .replace("__HOST__", public_url)
        .replace("__API_VERSION__", API_VERSION)
}

fn render_script(template: &str, public_url: &str, headers: &HeaderMap) -> Response {
    let body = script_body(template, public_url);
    http_cache::respond(
        headers,
        http_cache::Representation::new(body, "text/x-shellscript; charset=utf-8"),
    )
}

fn cached_hash(app: &App, key: &str, bytes: &[u8]) -> String {
    let mut cache = app.hashes.lock().unwrap();
    if let Some(hash) = cache.get(key) {
        return hash.clone();
    }
    let hash = blake3::hash(bytes).to_hex().to_string();
    cache.insert(key.to_string(), hash.clone());
    hash
}

fn hash_body(app: &App, key: &str, bytes: &[u8]) -> Response {
    plain(StatusCode::OK, cached_hash(app, key, bytes))
}

pub async fn docs_hash(State(app): State<App>) -> Response {
    let key = format!("docs:{}", app.public_url);
    let body = app.pages.guide_plain().to_owned();
    hash_body(&app, &key, body.as_bytes())
}

pub async fn stats(State(app): State<App>) -> Result<Response, ApiError> {
    let stats = app.run_store(|store| store.stats()).await?;
    Ok(Json(stats).into_response())
}

macro_rules! page_handlers {
    ($($handler:ident => $special:ident),* $(,)?) => {$(
        pub async fn $handler(State(app): State<App>, headers: HeaderMap) -> Response {
            page::respond(&headers, &app.pages, page::Special::$special)
        }
    )*};
}

page_handlers! {
    docs => Guide,
    api_index => ApiIndex,
    api_javascript => ApiJavaScript,
    api_python => ApiPython,
    api_shell => ApiShell,
    api_protocol => ApiProtocol,
    api_markdown => MarkdownGuide,
}

pub async fn api_redirect() -> Redirect {
    Redirect::temporary("/API/")
}

/// The guide's Markdown whatever the client asks for, as `RAW` is for a file.
pub async fn api_markdown_raw(State(app): State<App>, headers: HeaderMap) -> Response {
    page::respond_raw(&headers, &app.pages, page::Special::MarkdownGuide)
}

pub async fn api_version(headers: HeaderMap) -> Response {
    generated_asset_response(
        &headers,
        &API_VERSION_DOCUMENT,
        "application/json; charset=utf-8",
    )
}

pub async fn api_manual_not_found() -> Response {
    plain(StatusCode::NOT_FOUND, "error: API manual not found\n")
}

pub async fn api_manual_method_not_allowed() -> Response {
    let mut response = plain(
        StatusCode::METHOD_NOT_ALLOWED,
        "error: the built-in API site is read-only\n",
    );
    response
        .headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("GET, HEAD"));
    response
}

fn generated_asset_response(
    headers: &HeaderMap,
    body: &'static str,
    content_type: &'static str,
) -> Response {
    http_cache::respond(headers, http_cache::Representation::new(body, content_type))
}

macro_rules! hash_handler {
    ($hash_handler:ident, $body:ident, $key:literal) => {
        pub async fn $hash_handler(State(app): State<App>) -> Response {
            hash_body(&app, $key, $body.as_bytes())
        }
    };
}

macro_rules! generated_asset_handlers {
    ($handler:ident, $hash_handler:ident, $body:ident, $content_type:literal, $key:literal) => {
        pub async fn $handler(headers: HeaderMap) -> Response {
            generated_asset_response(&headers, $body, $content_type)
        }

        hash_handler!($hash_handler, $body, $key);
    };
}

macro_rules! script_handlers {
    ($handler:ident, $hash_handler:ident, $body:ident, $key:literal) => {
        pub async fn $handler(State(app): State<App>, headers: HeaderMap) -> Response {
            render_script($body, &app.public_url, &headers)
        }

        hash_handler!($hash_handler, $body, $key);
    };
}

generated_asset_handlers!(
    api_ts,
    api_ts_hash,
    API_TS,
    "text/typescript; charset=utf-8",
    "symbol.ts"
);
generated_asset_handlers!(
    api_js,
    api_js_hash,
    API_JS,
    "text/javascript; charset=utf-8",
    "symbol.js"
);
generated_asset_handlers!(
    api_global_js,
    api_global_js_hash,
    API_GLOBAL_JS,
    "text/javascript; charset=utf-8",
    "symbol.global.js"
);
generated_asset_handlers!(
    api_d_ts,
    api_d_ts_hash,
    API_D_TS,
    "text/typescript; charset=utf-8",
    "symbol.d.ts"
);
generated_asset_handlers!(
    api_py,
    api_py_hash,
    API_PY,
    "text/x-python; charset=utf-8",
    "symbol.py"
);

script_handlers!(install_sh, install_sh_hash, INSTALL_SH, "install.sh");
script_handlers!(symbol_sh, symbol_sh_hash, SYMBOL_SH, "symbol.sh");
