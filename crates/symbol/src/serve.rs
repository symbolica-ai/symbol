//! Reading sites: listings, files, rendered Markdown and hashes.

use axum::Json;
use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};

use crate::api_error::ApiError;
use crate::app::App;
use crate::blob::{add_target_expiry_headers, send_blob_file, send_expiring_blob};
use crate::hash::ContentHash;
use crate::lifecycle::expiry_path_report;
use crate::response::plain;
use crate::store::{Store, StoreError};
use crate::{assets, browse, http_cache, markdown, markdown_cache, pathutil, store};

fn strip_hash_path(path: &str) -> Option<&str> {
    let path = path.trim_end_matches('/');
    if path == "HASH" {
        return Some("");
    }
    path.strip_suffix("/HASH")
}

async fn send_hash(app: &App, name: &str, rel: &str) -> Result<Response, ApiError> {
    let name = name.to_string();
    let rel = rel.to_string();
    let hash = app
        .run_store(move |store| lookup_hash(&store, &name, &rel))
        .await?
        .ok_or(StoreError::NotFound)?;
    Ok(plain(StatusCode::OK, hash.to_hex()))
}

fn lookup_hash(store: &Store, name: &str, rel: &str) -> Result<Option<ContentHash>, StoreError> {
    if rel.is_empty() {
        return Ok(["index.html", "index.htm"].iter().find_map(|index| {
            store
                .child_blob(name, "", index)
                .ok()
                .and_then(|node| match node {
                    store::Node::File { hash, .. } => Some(hash),
                    store::Node::Dir => None,
                })
        }));
    }
    match store.lookup(name, rel) {
        Ok(store::Node::File { hash, .. }) => Ok(Some(hash)),
        Ok(store::Node::Dir) | Err(StoreError::NotFound) => Ok(None),
        Err(err) => Err(err),
    }
}

pub async fn browse_root(
    State(app): State<App>,
    Path(name): Path<String>,
    OriginalUri(uri): OriginalUri,
    Query(query): Query<browse::ListingQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(',')
                .any(|part| part.trim() == "application/json")
        })
    {
        let inventory = app
            .run_store({
                let name = name.clone();
                move |store| store.site_inventory(&name)
            })
            .await?;
        let revision = inventory.content_revision;
        let etag = inventory.tree_hash.clone();
        let mut response = Json(inventory).into_response();
        let response_headers = response.headers_mut();
        response_headers.insert(
            header::ETAG,
            HeaderValue::from_str(&format!("\"{etag}\"")).expect("valid site ETag"),
        );
        response_headers.insert(
            "content-revision",
            HeaderValue::from_str(&revision.to_string()).expect("valid revision"),
        );
        response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        return Ok(response);
    }
    let sort = match browse::Sort::parse(&query) {
        Ok(sort) => sort,
        Err(message) => return Ok(browse::bad_query(message)),
    };
    if browse::wants_tsv(&headers) {
        let request = match browse::TsvRequest::parse(&uri, &query) {
            Ok(request) => request,
            Err(message) => return Ok(browse::bad_query(message)),
        };
        let paging = request.paging;
        if !query.recursive() {
            let view = browse::ListingView {
                sort,
                tsv: Some(request),
            };
            return browse_dir(&app, &name, "", true, &headers, view).await;
        }
        let inventory = app
            .run_store({
                let name = name.clone();
                move |store| store.site_inventory(&name)
            })
            .await?;
        let page = browse::inventory_tsv(&inventory, paging);
        let total = page.total;
        let mut response = (
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static(browse::TSV_TYPE),
            )],
            page.body,
        )
            .into_response();
        let response_headers = response.headers_mut();
        response_headers.insert(
            header::ETAG,
            HeaderValue::from_str(&format!("\"{}\"", inventory.tree_hash))
                .expect("valid site ETag"),
        );
        response_headers.insert(
            "content-revision",
            HeaderValue::from_str(&inventory.content_revision.to_string()).expect("valid revision"),
        );
        response_headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
        response_headers.insert(header::VARY, HeaderValue::from_static("Accept"));
        browse::add_paging_headers(&mut response, &uri, paging, total);
        return Ok(response);
    }
    let view = browse::ListingView { sort, tsv: None };
    browse_dir(&app, &name, "", true, &headers, view).await
}

pub async fn browse_path(
    State(app): State<App>,
    Path((name, path)): Path<(String, String)>,
    OriginalUri(uri): OriginalUri,
    Query(query): Query<browse::ListingQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let rel = path.trim_end_matches('/');
    let node = app
        .run_store({
            let name = name.clone();
            let rel = rel.to_string();
            move |store| store.lookup(&name, &rel)
        })
        .await?;
    match node {
        store::Node::Dir => {
            if !path.is_empty() && !path.ends_with('/') {
                let query = uri
                    .query()
                    .map(|query| format!("?{query}"))
                    .unwrap_or_default();
                return Ok(
                    Redirect::temporary(&format!("/{name}/FILES/{path}/{query}")).into_response(),
                );
            }
            let sort = match browse::Sort::parse(&query) {
                Ok(sort) => sort,
                Err(message) => return Ok(browse::bad_query(message)),
            };
            let tsv = if browse::wants_tsv(&headers) {
                match browse::TsvRequest::parse(&uri, &query) {
                    Ok(request) => Some(request),
                    Err(message) => return Ok(browse::bad_query(message)),
                }
            } else {
                None
            };
            browse_dir(
                &app,
                &name,
                rel,
                true,
                &headers,
                browse::ListingView { sort, tsv },
            )
            .await
        }
        store::Node::File { .. } => {
            let path = app
                .run_store({
                    let name = name.clone();
                    let rel = rel.to_string();
                    move |store| Ok(pretty_html_rel(&store, &name, &rel)?.unwrap_or(rel))
                })
                .await?;
            Ok(Redirect::temporary(&format!("/{name}/{path}")).into_response())
        }
    }
}

pub async fn browse_dir(
    app: &App,
    name: &str,
    rel: &str,
    files_view: bool,
    headers: &HeaderMap,
    view: browse::ListingView<'_>,
) -> Result<Response, ApiError> {
    let entries = app
        .run_store({
            let name = name.to_string();
            let rel = rel.to_string();
            move |store| store.list_dir(&name, &rel)
        })
        .await?;
    Ok(browse::listing(
        headers, name, rel, entries, files_view, view,
    ))
}

pub async fn serve_index(
    State(app): State<App>,
    Path(name): Path<String>,
    Query(query): Query<browse::ListingQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    serve_from(&app, &name, "", &query, &headers).await
}

pub async fn serve_path(
    State(app): State<App>,
    Path((name, path)): Path<(String, String)>,
    Query(query): Query<browse::ListingQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let control_path = path.trim_end_matches('/');
    if let Some(rel) = control_path.strip_suffix("/EXPIRES") {
        return expiry_path_report(&app, name, rel.to_string()).await;
    }
    if let Some(rel) = strip_hash_path(&path) {
        return send_hash(&app, &name, rel).await;
    }
    if let Some(rel) = strip_raw_path(&path) {
        return send_raw(&app, &name, rel, &headers).await;
    }
    serve_from(&app, &name, &path, &query, &headers).await
}

fn strip_raw_path(path: &str) -> Option<&str> {
    let path = path.trim_end_matches('/');
    if path == "RAW" {
        return Some("");
    }
    path.strip_suffix("/RAW")
}

/// Serves exactly the stored bytes at `rel`, with nothing layered on top.
///
/// The ordinary read path is allowed to be clever: it tries `.html` fallbacks,
/// redirects to pretty URLs, resolves directory indexes, and renders markdown
/// for browsers. `RAW` does none of that. It resolves `rel` the same way `HASH`
/// does, so for any file the bytes it returns hash to what `HASH` reports.
///
/// A directory, including the site root, has no raw bytes and is `404`: mapping
/// it to an index file would be exactly the augmentation `RAW` exists to avoid.
async fn send_raw(
    app: &App,
    name: &str,
    rel: &str,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    if rel.is_empty() {
        return Err(StoreError::NotFound.into());
    }
    let node = app
        .run_store({
            let name = name.to_string();
            let rel = rel.to_string();
            move |store| store.lookup(&name, &rel)
        })
        .await?;
    match node {
        store::Node::File { logical, hash } => {
            send_expiring_blob(headers, name, &logical, hash, app).await
        }
        store::Node::Dir => Err(StoreError::NotFound.into()),
    }
}

async fn serve_from(
    app: &App,
    name: &str,
    rel: &str,
    query: &browse::ListingQuery,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    let node = app
        .run_store({
            let name = name.to_string();
            let rel = rel.to_string();
            move |store| lookup_site_get_redirecting(&store, &name, &rel)
        })
        .await?;
    match node {
        SiteGet::Redirect(pretty) => {
            Ok(Redirect::temporary(&format!("/{name}/{pretty}")).into_response())
        }
        SiteGet::Node(store::Node::Dir) => {
            if !rel.is_empty() && !rel.ends_with('/') {
                return Ok(Redirect::temporary(&format!("/{name}/{rel}/")).into_response());
            }
            let index = app
                .run_store({
                    let name = name.to_string();
                    let rel = rel.to_string();
                    move |store| find_index(&store, &name, &rel)
                })
                .await?;
            if let Some((logical, hash)) = index {
                return send_expiring_blob(headers, name, &logical, hash, app).await;
            }
            // A directory without an index lists itself, and sorts like any
            // other listing. A file ignores the query entirely.
            let sort = match browse::Sort::parse(query) {
                Ok(sort) => sort,
                Err(message) => return Ok(browse::bad_query(message)),
            };
            let view = browse::ListingView { sort, tsv: None };
            let mut response = browse_dir(app, name, rel, false, headers, view).await?;
            add_target_expiry_headers(app, name, rel, response.headers_mut()).await;
            Ok(response)
        }
        SiteGet::Node(store::Node::File { logical, hash }) => {
            send_site_file(headers, name, &logical, hash, app).await
        }
    }
}

/// The ordinary file read, which may render Markdown for a browser.
async fn send_site_file(
    headers: &HeaderMap,
    name: &str,
    logical: &str,
    hash: ContentHash,
    app: &App,
) -> Result<Response, ApiError> {
    if !markdown::is_markdown(logical) {
        return send_expiring_blob(headers, name, logical, hash, app).await;
    }
    if markdown::wants_rendering(headers)
        && let Some(response) = send_rendered_markdown(headers, name, logical, hash, app).await
    {
        return Ok(response);
    }
    let mut response = send_expiring_blob(headers, name, logical, hash, app).await?;
    // One URL, two representations chosen by `Accept`: caches must key on it
    // for the source as well as for the rendering, or a shared cache could
    // hand a browser's HTML to curl.
    response
        .headers_mut()
        .insert(header::VARY, HeaderValue::from_static("Accept"));
    Ok(response)
}

/// Reads and renders a file the cache did not have, and caches the result.
async fn render_markdown_file(
    app: &App,
    hash: ContentHash,
    logical: &str,
    raw_href: &str,
    key: markdown_cache::Key,
) -> Option<axum::body::Bytes> {
    let size = tokio::fs::metadata(app.store.blob_path(hash))
        .await
        .ok()?
        .len();
    if size > markdown::RENDER_LIMIT_BYTES {
        return None;
    }
    let bytes = app
        .run_store(move |store| store.read_blob(hash))
        .await
        .ok()?;
    let path = logical.to_string();
    let href = raw_href.to_string();
    // Rendering a large document takes real CPU time; keep it off the threads
    // that serve every other request.
    let body = tokio::task::spawn_blocking(move || {
        let source = std::str::from_utf8(&bytes).ok()?;
        let assets = assets::base();
        Some(axum::body::Bytes::from(markdown::render(&markdown::Page {
            source,
            path: &path,
            raw_href: &href,
            assets: &assets,
        })))
    })
    .await
    .ok()??;
    markdown_cache::CACHE.insert(key, body.clone());
    Some(body)
}

/// Renders a Markdown file, or `None` to fall back to its source.
///
/// Falls back rather than failing for anything that cannot sensibly be read
/// as a document: a file over [`markdown::RENDER_LIMIT_BYTES`], or one that is
/// not UTF-8.
///
/// The `ETag` names the rendering's inputs rather than its bytes, so a
/// matching `If-None-Match` is answered before reading or rendering anything.
/// Only a file that rendered can have produced that tag, and the tag covers
/// its content hash, so the answer cannot be stale.
async fn send_rendered_markdown(
    headers: &HeaderMap,
    name: &str,
    logical: &str,
    hash: ContentHash,
    app: &App,
) -> Option<Response> {
    let key = markdown_cache::key(hash, name, logical);
    let etag = markdown_cache::etag(&key);
    let raw_href = markdown::raw_href(name, logical);
    let vary = HeaderValue::from_static("Accept");
    if let Some(mut response) =
        http_cache::not_modified(headers, &etag, http_cache::Policy::Revalidate, Some(&vary))
    {
        add_target_expiry_headers(app, name, logical, response.headers_mut()).await;
        return Some(response);
    }

    let body = match markdown_cache::CACHE.get(&key) {
        Some(body) => body,
        None => render_markdown_file(app, hash, logical, &raw_href, key).await?,
    };

    let length = body.len();
    let mut representation = http_cache::Representation::new(body, "text/html; charset=utf-8");
    representation.etag = Some(etag);
    representation.vary = Some(vary);
    representation.link = HeaderValue::from_str(&format!(
        "<{raw_href}>; rel=\"alternate\"; type=\"text/markdown\""
    ))
    .ok();
    let mut response = http_cache::respond(headers, representation);
    if response.status() == StatusCode::OK {
        let response_headers = response.headers_mut();
        response_headers.insert(
            header::CONTENT_LENGTH,
            HeaderValue::from_str(&length.to_string()).expect("valid content length"),
        );
        // A rendering is generated, so byte ranges into it mean nothing.
        response_headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("none"));
    }
    add_target_expiry_headers(app, name, logical, response.headers_mut()).await;
    Some(response)
}

/// Serves a built-in asset that rendered pages reference.
pub async fn render_asset(
    Path((bundle, path)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let asset = assets::find(&bundle, &path).ok_or(StoreError::NotFound)?;
    let mut representation = http_cache::Representation::new(
        axum::body::Bytes::from_static(asset.bytes),
        asset.content_type,
    );
    representation.policy = http_cache::Policy::Immutable;
    representation.nosniff = true;
    Ok(http_cache::respond(&headers, representation))
}

enum SiteGet {
    Redirect(String),
    Node(store::Node),
}

fn lookup_site_get_redirecting(
    store: &Store,
    name: &str,
    rel: &str,
) -> Result<SiteGet, StoreError> {
    if let Some(pretty) = pretty_html_rel(store, name, rel)? {
        return Ok(SiteGet::Redirect(pretty));
    }
    lookup_with_html_fallback(store, name, rel).map(SiteGet::Node)
}

fn pretty_html_rel(store: &Store, site: &str, rel: &str) -> Result<Option<String>, StoreError> {
    let (dir, name) = match rel.rsplit_once('/') {
        Some((dir, name)) => (dir, name),
        None => ("", rel),
    };
    let Some((stem, suffix)) = pathutil::html_suffix(name) else {
        return Ok(None);
    };
    match store.lookup(site, rel) {
        Ok(store::Node::File { .. }) => {}
        Ok(store::Node::Dir) | Err(StoreError::NotFound) => return Ok(None),
        Err(err) => return Err(err),
    }
    let pretty = if dir.is_empty() {
        stem.to_string()
    } else {
        format!("{dir}/{stem}")
    };
    if path_claimed(store, site, &pretty)? {
        return Ok(None);
    }
    if suffix == pathutil::HtmlSuffix::Htm && path_claimed(store, site, &format!("{pretty}.html"))?
    {
        return Ok(None);
    }
    Ok(Some(pretty))
}

fn path_claimed(store: &Store, site: &str, rel: &str) -> Result<bool, StoreError> {
    match store.lookup(site, rel) {
        Ok(_) => Ok(true),
        Err(StoreError::NotFound) => match store.alias(site, rel) {
            Ok(_) => Ok(true),
            Err(StoreError::NotFound) => Ok(false),
            Err(err) => Err(err),
        },
        Err(err) => Err(err),
    }
}

fn lookup_with_html_fallback(
    store: &Store,
    name: &str,
    rel: &str,
) -> Result<store::Node, StoreError> {
    match store.lookup(name, rel) {
        Ok(node) => return Ok(node),
        Err(StoreError::NotFound)
            if !rel.is_empty()
                && !rel.ends_with('/')
                && !std::path::Path::new(rel)
                    .extension()
                    .and_then(std::ffi::OsStr::to_str)
                    .is_some_and(|extension| {
                        extension.eq_ignore_ascii_case("html")
                            || extension.eq_ignore_ascii_case("htm")
                    }) => {}
        Err(err) => return Err(err),
    }
    for suffix in [".html", ".htm"] {
        match store.lookup(name, &format!("{rel}{suffix}")) {
            Ok(node @ store::Node::File { .. }) => return Ok(node),
            Ok(store::Node::Dir) | Err(StoreError::NotFound) => {}
            Err(err) => return Err(err),
        }
    }
    Err(StoreError::NotFound)
}

fn find_index(
    store: &Store,
    name: &str,
    rel: &str,
) -> Result<Option<(String, ContentHash)>, StoreError> {
    for index in ["index.html", "index.htm"] {
        match store.child_blob(name, rel, index) {
            Ok(store::Node::File { logical, hash }) => return Ok(Some((logical, hash))),
            Ok(store::Node::Dir) | Err(StoreError::NotFound) => {}
            Err(err) => return Err(err),
        }
    }
    Ok(None)
}

pub async fn serve_immutable_blob(
    State(app): State<App>,
    Path((name, hash)): Path<(String, String)>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let hash = ContentHash::parse_wire(&hash).map_err(|_| StoreError::NotFound)?;
    let referenced = app
        .run_store({
            let name = name.clone();
            move |store| store.site_references_blob(&name, hash)
        })
        .await?;
    if !referenced {
        return Err(StoreError::NotFound.into());
    }
    send_blob_file(
        &headers,
        "application/octet-stream",
        hash,
        http_cache::Policy::Immutable,
        &app,
    )
    .await
}
