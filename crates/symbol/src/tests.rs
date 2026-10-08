use std::io;
use std::net::IpAddr;
use std::sync::{Arc, Mutex};

use super::*;
use axum::body::Body;
use axum::body::to_bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, Request};
use axum::middleware;
use axum::response::IntoResponse;
use bytes::Bytes as ByteChunk;
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use tower::ServiceExt as _;

const CREATOR_CLAIM: &str =
    "sym_claim_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn test_app(store: Store) -> App {
    App::new(store)
}

fn assert_contract_status(name: &str, status: StatusCode) {
    let endpoint = contract::ENDPOINTS
        .iter()
        .find(|endpoint| endpoint.name == name)
        .unwrap_or_else(|| panic!("missing contract {name}"));
    let code = status.as_u16();
    assert!(
        endpoint.has_status(code),
        "{name} does not declare observed status {code}"
    );
}

/// A fresh store; keep the returned directory alive for as long as the store is used.
fn temp_store() -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().to_path_buf()).unwrap();
    (root, store)
}

fn put_text_files(store: &Store, site: &str, files: &[(&str, &str)]) {
    for (path, text) in files {
        store.put_file(site, path, text.as_bytes()).unwrap();
    }
}

fn app_router(store: &Store) -> Router {
    router(test_app(store.clone()))
}

fn file_hash(store: &Store, site: &str, path: &str) -> ContentHash {
    match store.lookup(site, path).unwrap() {
        store::Node::File { hash, .. } => hash,
        store::Node::Dir => panic!("expected file at {site}/{path}"),
    }
}

fn tmp_dir_is_empty(root: &std::path::Path) -> bool {
    std::fs::read_dir(root.join("tmp"))
        .unwrap()
        .next()
        .is_none()
}

fn build_request(method: &str, uri: &str, headers: &[(&str, &str)], body: Body) -> Request<Body> {
    let mut request = Request::builder().method(method).uri(uri);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    request.body(body).unwrap()
}

async fn send_body(
    app: &Router,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Body,
) -> Response {
    app.clone()
        .oneshot(build_request(method, uri, headers, body))
        .await
        .unwrap()
}

async fn send(app: &Router, method: &str, uri: &str, headers: &[(&str, &str)]) -> Response {
    send_body(app, method, uri, headers, Body::empty()).await
}

async fn get_with(app: &Router, path: &str, headers: &[(&str, &str)]) -> Response {
    send(app, "GET", path, headers).await
}

/// Sends a request that appears to come from the socket peer `peer`.
async fn send_from_peer(
    app: &Router,
    peer: impl Into<SocketAddr>,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Body,
) -> Response {
    let mut request = build_request(method, uri, headers, body);
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(peer.into()));
    app.clone().oneshot(request).await.unwrap()
}

async fn body_bytes(response: Response) -> Vec<u8> {
    to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec()
}

async fn body_string(response: Response) -> String {
    String::from_utf8(body_bytes(response).await).unwrap()
}

async fn body_json<T: serde::de::DeserializeOwned>(response: Response) -> T {
    serde_json::from_slice(&body_bytes(response).await).unwrap()
}

fn header_string(response: &Response, name: &str) -> String {
    response.headers()[name].to_str().unwrap().to_string()
}

fn assert_header(response: &Response, name: &str, expected: &str) {
    assert_eq!(response.headers()[name], expected, "{name}");
}

fn bearer(token: &str) -> String {
    format!("Bearer {token}")
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn loopback_peers() -> Arc<[IpAddr]> {
    Arc::from([IpAddr::from([127, 0, 0, 1])])
}

fn identity_router(store: Store, provider: IdentityProvider) -> Router {
    router(App::with_options(
        store,
        DEFAULT_MAX_FILE_SIZE,
        DEFAULT_MAX_ARCHIVE_UPLOAD,
        "http://symbol".into(),
        provider,
    ))
}

fn latest_audit_source_ip(root: &std::path::Path, site: &str) -> Option<String> {
    let mut db = SqliteConnection::establish(&root.join("symbol.db").to_string_lossy()).unwrap();
    crate::schema::management_audit::table
        .filter(crate::schema::management_audit::site_name.eq(site))
        .select(crate::schema::management_audit::source_ip)
        .order(crate::schema::management_audit::id.desc())
        .first::<Option<String>>(&mut db)
        .unwrap()
}

#[test]
fn text_media_types_are_labelled_utf8_unless_they_self_declare() {
    for (given, expected) in [
        ("text/markdown", "text/markdown; charset=utf-8"),
        ("text/plain", "text/plain; charset=utf-8"),
        ("text/css", "text/css; charset=utf-8"),
        ("text/javascript", "text/javascript; charset=utf-8"),
        ("text/csv", "text/csv; charset=utf-8"),
        ("TEXT/Plain", "TEXT/Plain; charset=utf-8"),
        // An explicit charset, in any case, is the author's and wins.
        (
            "text/plain; charset=iso-8859-1",
            "text/plain; charset=iso-8859-1",
        ),
        ("text/plain;CHARSET=utf-16", "text/plain;CHARSET=utf-16"),
        // Self-declaring formats: an HTTP charset would override `<meta>`.
        ("text/html", "text/html"),
        ("text/xml", "text/xml"),
        // Not text at all.
        ("application/json", "application/json"),
        ("image/png", "image/png"),
        ("application/octet-stream", "application/octet-stream"),
        ("image/svg+xml", "image/svg+xml"),
    ] {
        assert_eq!(with_default_charset(given), expected, "{given}");
    }
}

#[tokio::test]
async fn served_text_files_carry_a_charset() {
    let (_root, store) = temp_store();
    put_text_files(
        &store,
        "hello",
        &[
            ("notes.md", "# café ✓\n"),
            ("page.html", "<meta charset=utf-8>"),
            ("plain.html", "<p>café ✓</p>"),
        ],
    );
    store
        .put_file("hello", "legacy.html", b"<p>caf\xE9</p>")
        .unwrap();
    let app = app_router(&store);

    let response = get_with(&app, "/hello/notes.md", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_header(&response, "content-type", "text/markdown; charset=utf-8");

    // A page's own declaration is left to win.
    let response = get_with(&app, "/hello/page", &[]).await;
    assert_header(&response, "content-type", "text/html");

    // Declaring nothing and UTF-8 throughout: labelled, raw bytes too.
    for path in ["/hello/plain", "/hello/plain.html/RAW"] {
        let response = get_with(&app, path, &[]).await;
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8",
            "{path}"
        );
    }

    // Declaring nothing and not UTF-8: the browser still decides.
    let response = get_with(&app, "/hello/legacy", &[]).await;
    assert_header(&response, "content-type", "text/html");
}

#[tokio::test]
async fn raw_returns_exactly_the_bytes_hash_describes() {
    let (_root, store) = temp_store();
    let source = "# Title\n\nSome *markdown* with café.\n";
    put_text_files(&store, "hello", &[("notes.md", source)]);
    let app = app_router(&store);

    let raw = get_with(&app, "/hello/notes.md/RAW", &[]).await;
    assert_eq!(raw.status(), StatusCode::OK);
    assert_header(&raw, "content-type", "text/markdown; charset=utf-8");
    let raw_bytes = body_bytes(raw).await;
    assert_eq!(raw_bytes, source.as_bytes());

    let hash = body_string(get_with(&app, "/hello/notes.md/HASH", &[]).await).await;
    assert_eq!(hash.trim(), blake3::hash(&raw_bytes).to_hex().as_str());
}

#[tokio::test]
async fn raw_is_never_augmented() {
    let (_root, store) = temp_store();
    put_text_files(
        &store,
        "hello",
        &[
            ("about.html", "<p>about</p>"),
            ("index.html", "<p>home</p>"),
        ],
    );
    let app = app_router(&store);
    let browser = [
        ("accept", "text/html,application/xhtml+xml,*/*;q=0.8"),
        ("user-agent", "Mozilla/5.0 (X11; Linux x86_64) Chrome/120"),
    ];

    // The ordinary read path redirects this to the pretty URL.
    let pretty = get_with(&app, "/hello/about.html", &browser).await;
    assert_eq!(pretty.status(), StatusCode::TEMPORARY_REDIRECT);
    // RAW serves the exact path instead.
    let exact = get_with(&app, "/hello/about.html/RAW", &browser).await;
    assert_eq!(exact.status(), StatusCode::OK);
    assert_eq!(body_bytes(exact).await, b"<p>about</p>");

    // The ordinary read path falls back from `about` to `about.html`.
    let fallback = get_with(&app, "/hello/about", &browser).await;
    assert_eq!(fallback.status(), StatusCode::OK);
    // RAW does not.
    let missing = get_with(&app, "/hello/about/RAW", &browser).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    // Nor does it resolve a directory to its index.
    let root_raw = get_with(&app, "/hello/RAW", &browser).await;
    assert_eq!(root_raw.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        get_with(&app, "/hello/RAW/", &browser).await.status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn raw_supports_ranges_and_revalidation() {
    let (_root, store) = temp_store();
    store.put_file("hello", "data.txt", b"abcdefgh").unwrap();
    let app = app_router(&store);

    let partial = get_with(&app, "/hello/data.txt/RAW", &[("range", "bytes=2-4")]).await;
    assert_eq!(partial.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(body_bytes(partial).await, b"cde");

    let full = get_with(&app, "/hello/data.txt/RAW", &[]).await;
    let etag = header_string(&full, "etag");
    let cached = get_with(&app, "/hello/data.txt/RAW", &[("if-none-match", &etag)]).await;
    assert_eq!(cached.status(), StatusCode::NOT_MODIFIED);
}

const BROWSER: [(&str, &str); 2] = [
    (
        "accept",
        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
    ),
    (
        "user-agent",
        "Mozilla/5.0 (Macintosh) AppleWebKit/537.36 Chrome/120",
    ),
];

fn markdown_app() -> Router {
    let root = tempfile::tempdir().unwrap().keep();
    let store = Store::new(root).unwrap();
    put_text_files(
        &store,
        "hello",
        &[(
            "docs/notes.md",
            "---\ntitle: Notes\n---\n# Heading\n\nSome $x^2$ math.\n",
        )],
    );
    app_router(&store)
}

#[tokio::test]
async fn browsers_get_rendered_markdown() {
    let app = markdown_app();
    let response = get_with(&app, "/hello/docs/notes.md", &BROWSER).await;
    assert_eq!(response.status(), StatusCode::OK);
    let headers = response.headers().clone();
    assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
    assert_eq!(headers[header::VARY], "Accept");
    assert_eq!(headers[header::ACCEPT_RANGES], "none");
    assert_eq!(
        headers[header::LINK],
        "</hello/docs/notes.md/RAW>; rel=\"alternate\"; type=\"text/markdown\""
    );
    // Still an ordinary `site file` 200 as far as the contract is concerned.
    assert_contract_status("site file", response.status());
    for required in [
        "content-type",
        "content-length",
        "etag",
        "cache-control",
        "accept-ranges",
    ] {
        assert!(headers.contains_key(required), "missing {required}");
    }
    let body = body_string(response).await;
    assert_eq!(headers[header::CONTENT_LENGTH], body.len().to_string());
    assert!(body.contains("<title>Notes</title>"));
    assert!(body.contains(r#"<span class="math math-inline">x^2</span>"#));
    assert!(body.contains(r#"href="/hello/docs/notes.md/RAW""#));
}

#[tokio::test]
async fn rendered_markdown_is_cached_and_keyed_by_content_hash() {
    let (_root, store) = temp_store();
    store.put_file("cache", "page.md", b"# First\n").unwrap();
    let app = app_router(&store);

    let first_hash = file_hash(&store, "cache", "page.md");
    let key = markdown_cache::key(first_hash, "cache", "page.md");
    let first = get_with(&app, "/cache/page.md", &BROWSER).await;
    let etag = header_string(&first, "etag");
    assert_eq!(
        etag,
        markdown_cache::etag(&key),
        "the ETag names the inputs"
    );
    assert!(markdown_cache::is_cached(&key), "the rendering was kept");
    let again = get_with(&app, "/cache/page.md", &BROWSER).await;
    assert_eq!(again.headers()[header::ETAG], etag.as_str());
    assert!(body_string(again).await.contains("First"));

    let revalidated = get_with(
        &app,
        "/cache/page.md",
        &[BROWSER[0], ("if-none-match", &etag)],
    )
    .await;
    assert_eq!(revalidated.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(revalidated.headers()[header::VARY], "Accept");

    // Changing the file changes its hash, so the old tag stops matching.
    store.put_file("cache", "page.md", b"# Second\n").unwrap();
    assert_ne!(file_hash(&store, "cache", "page.md"), first_hash);
    let changed = get_with(
        &app,
        "/cache/page.md",
        &[BROWSER[0], ("if-none-match", &etag)],
    )
    .await;
    assert_eq!(changed.status(), StatusCode::OK);
    assert_ne!(changed.headers()[header::ETAG], etag.as_str());
    assert!(body_string(changed).await.contains("Second"));
}

#[tokio::test]
async fn revalidating_a_rendering_reads_and_renders_nothing() {
    let (_root, store) = temp_store();
    store
        .put_file("cold", "never-rendered.md", b"# Cold\n")
        .unwrap();
    let hash = file_hash(&store, "cold", "never-rendered.md");
    // With the stored bytes gone, anything that tried to read or render
    // would fail; the conditional request must not need them.
    std::fs::remove_file(store.blob_path(hash)).unwrap();
    let app = app_router(&store);
    let etag = markdown_cache::etag(&markdown_cache::key(hash, "cold", "never-rendered.md"));
    let response = get_with(
        &app,
        "/cold/never-rendered.md",
        &[BROWSER[0], ("if-none-match", &etag)],
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(response.headers()[header::ETAG], etag.as_str());
}

#[tokio::test]
async fn the_markdown_guide_is_served_under_api() {
    let app = markdown_app();
    let page = get_with(&app, "/API/MARKDOWN", &BROWSER).await;
    assert_eq!(page.status(), StatusCode::OK);
    assert_header(&page, "content-type", "text/html; charset=utf-8");
    assert_header(&page, "link", "</API/MARKDOWN>; rel=\"canonical\"");
    assert_contract_status("api documentation", page.status());
    let etag = page.headers()[header::ETAG].clone();
    let html = body_string(page).await;
    assert!(html.contains(r#"<article class="markdown-body">"#));

    let alias = get_with(&app, "/API/MD", &BROWSER).await;
    assert_eq!(alias.headers()[header::ETAG], etag, "MD is an alias");

    let markdown = get_with(&app, "/API/MARKDOWN", &[("accept", "text/markdown")]).await;
    assert_header(&markdown, "content-type", "text/markdown; charset=utf-8");

    let raw = get_with(&app, "/API/MARKDOWN/RAW", &BROWSER).await;
    assert_eq!(raw.status(), StatusCode::OK);
    assert_header(&raw, "content-type", "text/markdown; charset=utf-8");
    assert!(body_bytes(raw).await.starts_with(b"---\n"));

    let rejected = send_body(&app, "PUT", "/API/MARKDOWN/RAW", &[], Body::from("x")).await;
    assert_eq!(rejected.status(), StatusCode::METHOD_NOT_ALLOWED);
}

fn tsv_rows(body: &str) -> Vec<Vec<String>> {
    body.lines()
        .map(|line| line.split('\t').map(str::to_string).collect())
        .collect()
}

#[tokio::test]
async fn listings_and_inventories_are_served_as_tsv() {
    let (_root, store) = temp_store();
    put_text_files(
        &store,
        "tsv",
        &[
            ("b dir/x y.txt", "hello"),
            ("b dir/deep/z.txt", "zz"),
            ("a.txt", "12"),
            ("caf\u{e9}.md", "#"),
        ],
    );
    store
        .put_alias(
            "tsv",
            "link",
            "a.txt",
            store::FileMutationOptions::default(),
        )
        .unwrap();
    let app = app_router(&store);
    let tsv = [("accept", "text/tab-separated-values")];

    // One directory: its folders, files, then aliases.
    let response = get_with(&app, "/tsv/FILES", &tsv).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_header(
        &response,
        "content-type",
        "text/tab-separated-values; charset=utf-8",
    );
    let rows = tsv_rows(&body_string(response).await);
    assert_eq!(rows[0], ["kind", "files", "bytes", "name", "target"]);
    let names: Vec<&str> = rows[1..].iter().map(|row| row[3].as_str()).collect();
    assert!(names.contains(&"b dir") && names.contains(&"a.txt") && names.contains(&"link"));
    assert!(
        !names.iter().any(|name| name.contains('/')),
        "one level only: {names:?}"
    );
    let dir = rows.iter().find(|row| row[3] == "b dir").unwrap();
    assert_eq!(
        (dir[0].as_str(), dir[1].as_str(), dir[2].as_str()),
        ("directory", "2", "7")
    );
    let alias = rows.iter().find(|row| row[3] == "link").unwrap();
    assert_eq!((alias[0].as_str(), alias[4].as_str()), ("alias", "a.txt"));

    // A subdirectory, and its trailing-slash redirect keeping the query.
    let sub = get_with(&app, "/tsv/FILES/b%20dir/", &tsv).await;
    let rows = tsv_rows(&body_string(sub).await);
    let names: Vec<&str> = rows[1..].iter().map(|row| row[3].as_str()).collect();
    assert_eq!(names, ["deep", "x y.txt"]);
    let redirect = get_with(&app, "/tsv/FILES/b%20dir?limit=1", &tsv).await;
    assert_eq!(redirect.status(), StatusCode::TEMPORARY_REDIRECT);
    assert!(
        redirect.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .ends_with("/?limit=1")
    );

    // ?recursive is the whole inventory, with the site identity headers.
    let response = get_with(&app, "/tsv/FILES?recursive", &tsv).await;
    for required in ["etag", "content-revision", "cache-control", "entry-count"] {
        assert!(response.headers().contains_key(required), "{required}");
    }
    let rows = tsv_rows(&body_string(response).await);
    assert_eq!(rows[0], ["kind", "size", "value", "path"]);
    let paths: Vec<&str> = rows[1..].iter().map(|row| row[3].as_str()).collect();
    assert_eq!(
        paths,
        [
            "a.txt",
            "b dir/deep/z.txt",
            "b dir/x y.txt",
            "caf\u{e9}.md",
            "link"
        ],
        "files sorted, symbol.toml excluded, aliases last"
    );
    assert!(rows[1][2].starts_with("blake3:"));

    let sites = get_with(&app, "/FILES", &tsv).await;
    let body = body_string(sites).await;
    assert!(
        body.starts_with("kind\tfiles\tbytes\tname\ttarget\nbuiltin\t\t0\tAPI\t\n"),
        "{body}"
    );
    assert!(
        body.lines()
            .any(|line| line.starts_with("site\t") && line.ends_with("\ttsv\t"))
    );

    let missing = get_with(&app, "/nope/FILES", &tsv).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn tsv_listings_page_with_limit_and_page() {
    let (_root, store) = temp_store();
    for index in 0..7 {
        store
            .put_file("paged", &format!("f{index}.txt"), b"x")
            .unwrap();
    }
    let app = app_router(&store);
    let tsv = [("accept", "text/tab-separated-values")];
    let names = |body: String| -> Vec<String> {
        tsv_rows(&body)[1..]
            .iter()
            .map(|row| row[3].clone())
            .collect()
    };

    // Seven files plus the generated symbol.toml.
    let first = get_with(&app, "/paged/FILES?limit=3", &tsv).await;
    assert_eq!(first.headers()["entry-count"], "8");
    assert_eq!(
        first.headers()[header::LINK],
        "</paged/FILES?limit=3&page=2>; rel=\"next\""
    );
    assert_eq!(
        names(body_string(first).await),
        ["f0.txt", "f1.txt", "f2.txt"]
    );

    let middle = get_with(&app, "/paged/FILES?limit=3&page=2", &tsv).await;
    assert_eq!(
        middle.headers()[header::LINK],
        "</paged/FILES?limit=3&page=1>; rel=\"prev\", </paged/FILES?limit=3&page=3>; rel=\"next\""
    );
    let last = get_with(&app, "/paged/FILES?limit=3&page=3", &tsv).await;
    assert_eq!(
        last.headers()[header::LINK],
        "</paged/FILES?limit=3&page=2>; rel=\"prev\""
    );
    assert_eq!(names(body_string(last).await), ["f6.txt", "symbol.toml"]);
    let beyond = get_with(&app, "/paged/FILES?limit=3&page=9", &tsv).await;
    assert_eq!(beyond.status(), StatusCode::OK);
    assert!(names(body_string(beyond).await).is_empty());

    let recursive = get_with(&app, "/paged/FILES?recursive&limit=2&page=2", &tsv).await;
    assert_eq!(
        recursive.headers()["entry-count"],
        "7",
        "the inventory excludes symbol.toml"
    );
    assert!(
        recursive.headers()[header::LINK]
            .to_str()
            .unwrap()
            .contains("recursive&limit=2&page=3")
    );

    let unpaged = get_with(&app, "/paged/FILES", &tsv).await;
    assert!(unpaged.headers().get(header::LINK).is_none());
    assert_eq!(unpaged.headers()["entry-count"], "8");

    for bad in [
        "limit=0",
        "limit=x",
        "page=2",
        "limit=3&page=0",
        "limit=100001",
    ] {
        let response = get_with(&app, &format!("/paged/FILES?{bad}"), &tsv).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{bad}");
    }
    // Paging parameters are ignored outside TSV, as before.
    let json = get_with(
        &app,
        "/paged/FILES?limit=0",
        &[("accept", "application/json")],
    )
    .await;
    assert_eq!(json.status(), StatusCode::OK);
}

#[tokio::test]
async fn control_characters_in_paths_are_rejected_not_fatal() {
    let app = markdown_app();
    for path in [
        "/hello/tab%09name.txt",
        "/hello/new%0Aline.txt",
        "/hello/esc%1B.txt",
    ] {
        let response = send_body(&app, "PUT", path, &[], Body::from("x")).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
    }
}

#[tokio::test]
async fn the_served_client_knows_its_api_version() {
    let app = markdown_app();
    let body = body_string(get_with(&app, "/symbol.sh", &[]).await).await;
    assert!(
        body.contains(&format!("CLIENT_API_VERSION='{API_VERSION}'")),
        "{body:.400}"
    );
    assert!(!body.contains("__API_VERSION__") && !body.contains("__HOST__"));
}

#[tokio::test]
async fn every_other_client_gets_the_source() {
    let app = markdown_app();
    let source = "---\ntitle: Notes\n---\n# Heading\n\nSome $x^2$ math.\n";
    for headers in [
        // curl
        &[("accept", "*/*"), ("user-agent", "curl/8.5.0")][..],
        // a browser `fetch()` sends `*/*` with a browser user agent
        &[("accept", "*/*"), BROWSER[1]][..],
        // asking for Markdown outright
        &[("accept", "text/markdown"), BROWSER[1]][..],
        &[][..],
    ] {
        let response = get_with(&app, "/hello/docs/notes.md", headers).await;
        assert_eq!(response.status(), StatusCode::OK, "{headers:?}");
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/markdown; charset=utf-8",
            "{headers:?}"
        );
        assert_eq!(response.headers()[header::VARY], "Accept");
        assert_eq!(body_bytes(response).await, source.as_bytes(), "{headers:?}");
    }
}

#[tokio::test]
async fn raw_never_renders_even_for_a_browser() {
    let app = markdown_app();
    let response = get_with(&app, "/hello/docs/notes.md/RAW", &BROWSER).await;
    assert_header(&response, "content-type", "text/markdown; charset=utf-8");
    assert!(body_bytes(response).await.starts_with(b"---\ntitle: Notes"));
}

#[tokio::test]
async fn oversized_or_non_utf8_markdown_is_served_raw() {
    let (_root, store) = temp_store();
    let limit = usize::try_from(markdown::RENDER_LIMIT_BYTES).unwrap();
    store
        .put_file("hello", "big.md", &vec![b'x'; limit + 1])
        .unwrap();
    store.put_file("hello", "latin1.md", b"caf\xe9\n").unwrap();
    let app = app_router(&store);
    for path in ["/hello/big.md", "/hello/latin1.md"] {
        let response = get_with(&app, path, &BROWSER).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/markdown; charset=utf-8",
            "{path}"
        );
    }
}

#[tokio::test]
async fn rendered_pages_reference_assets_that_resolve() {
    let app = markdown_app();
    let page = get_with(&app, "/hello/docs/notes.md", &BROWSER).await;
    let page = body_string(page).await;
    let mut checked = 0;
    for attribute in ["href=\"/ASSETS/", "src=\"/ASSETS/"] {
        for piece in page.split(attribute).skip(1) {
            let path = format!("/ASSETS/{}", piece.split('"').next().unwrap());
            let response = get_with(&app, &path, &[]).await;
            assert_eq!(response.status(), StatusCode::OK, "{path}");
            assert_header(
                &response,
                "cache-control",
                "public, max-age=31536000, immutable",
            );
            assert_header(&response, "x-content-type-options", "nosniff");
            checked += 1;
        }
    }
    assert!(
        checked >= 5,
        "expected css, katex css+js, highlight and page js; saw {checked}"
    );
}

#[tokio::test]
async fn assets_revalidate_and_reject_other_bundles() {
    let app = markdown_app();
    let path = format!("{}/markdown.css", assets::base());
    let first = get_with(&app, &path, &[]).await;
    assert_header(&first, "content-type", "text/css; charset=utf-8");
    let etag = header_string(&first, "etag");
    let cached = get_with(&app, &path, &[("if-none-match", &etag)]).await;
    assert_eq!(cached.status(), StatusCode::NOT_MODIFIED);

    let stale = get_with(&app, "/ASSETS/0000000000000000/markdown.css", &[]).await;
    assert_eq!(stale.status(), StatusCode::NOT_FOUND);
    let missing = get_with(&app, &format!("{}/nope.css", assets::base()), &[]).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let font = get_with(
        &app,
        &format!("{}/katex/fonts/KaTeX_Main-Regular.woff2", assets::base()),
        &[],
    )
    .await;
    assert_header(&font, "content-type", "font/woff2");
}

#[tokio::test]
async fn raw_is_a_reserved_path_component() {
    let (_root, store) = temp_store();
    store.put_file("hello", "index.html", b"home").unwrap();
    let app = app_router(&store);
    for path in ["/hello/RAW", "/hello/docs/RAW"] {
        let response = send_body(&app, "PUT", path, &[], Body::from("shadow")).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(
            body_bytes(response).await,
            format!("{}\n", contract::RESERVED_MUTATION_ERROR).as_bytes()
        );
    }
}

#[derive(Clone)]
struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

struct CapturedWriter(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for CapturedWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CapturedLogs {
    type Writer = CapturedWriter;

    fn make_writer(&'a self) -> Self::Writer {
        CapturedWriter(Arc::clone(&self.0))
    }
}

fn captured_text(captured: &Mutex<Vec<u8>>) -> String {
    String::from_utf8(captured.lock().unwrap().clone()).unwrap()
}

/// The `mutation_id=` field of the first log line mentioning `event`.
fn mutation_id<'a>(logs: &'a str, event: &str) -> &'a str {
    logs.lines()
        .find(|line| line.contains(event))
        .and_then(|line| {
            line.split_whitespace()
                .find_map(|field| field.strip_prefix("mutation_id="))
        })
        .unwrap()
}

#[test]
fn request_trace_span_never_records_secret_headers() {
    let request = build_request(
        "PUT",
        "/managed",
        &[
            ("authorization", "Bearer sym_mgmt_secret"),
            ("creator-claim", "sym_claim_secret"),
            ("idempotency-key", "secret-key"),
        ],
        Body::empty(),
    );
    let span = make_http_span(&request);
    let fields = span.metadata().unwrap().fields();
    assert!(fields.field("method").is_some());
    assert!(fields.field("uri").is_some());
    for secret in [
        "authorization",
        "creator_claim",
        "management_token",
        "idempotency_key",
    ] {
        assert!(fields.field(secret).is_none());
    }
}

#[test]
fn mutation_signals_cover_all_supported_write_methods() {
    for method in [Method::PUT, Method::DELETE, Method::POST, Method::PATCH] {
        assert!(is_mutation_method(&method), "{method}");
    }
    for method in CUSTOM_MUTATION_METHODS.iter() {
        assert!(is_mutation_method(method), "{method}");
    }
    for method in [Method::GET, Method::HEAD, Method::OPTIONS] {
        assert!(!is_mutation_method(&method), "{method}");
    }
}

#[tokio::test]
async fn typed_contract_matches_observed_lifecycle_dispatch() {
    let (_root, store) = temp_store();
    store.put_file("hello", "index.html", b"hello").unwrap();
    let app = app_router(&store);

    let get = get_with(&app, "/hello", &[]).await;
    assert_contract_status("site redirect", get.status());

    let put = send_body(&app, "PUT", "/hello/style.css", &[], Body::from("body{}")).await;
    assert_contract_status("file put", put.status());

    let copy = send(&app, "COPY", "/hello", &[("destination", "/copy")]).await;
    assert_contract_status("site copy", copy.status());

    let moved = send(&app, "MOVE", "/copy", &[("destination", "/moved")]).await;
    assert_contract_status("site move", moved.status());

    let expire = send(&app, "EXPIRE", "/hello", &[]).await;
    assert_contract_status("site expire", expire.status());

    let inventory = get_with(&app, "/hello/EXPIRES", &[]).await;
    assert_contract_status("expiry inventory", inventory.status());

    let management = send(&app, "MANAGE", "/hello", &[("management-action", "status")]).await;
    assert_contract_status("site management", management.status());
}

#[tokio::test]
async fn every_typed_contract_endpoint_observes_a_declared_status() {
    for endpoint in contract::ENDPOINTS {
        let (_root, store) = temp_store();
        let app = app_router(&store);
        let path = match endpoint.name {
            "docs" | "unnamed put" => "/",
            "docs hash" => "/HASH",
            "stats" => "/STATS",
            "installer" => "/install.sh",
            "installer hash" => "/install.sh/HASH",
            "client" => "/symbol.sh",
            "client hash" => "/symbol.sh/HASH",
            "api documentation" => "/API/JS",
            "api client asset" => "/symbol.js",
            "api client hash" => "/symbol.js/HASH",
            "api version" => "/API/VERSION",
            "site listing" => "/FILES",
            "site redirect" | "site put" | "site pop" | "site copy" | "site move" | "site undo"
            | "site expire" | "site management" => "/missing",
            "site index" | "alias batch" | "allocated file" => "/missing/",
            "site file" | "file put" | "file delete" | "file expire" | "alias file"
            | "file replace" | "file splice" => "/missing/file.txt",
            "archive get" | "archive pop" => "/missing.tar.gz",
            "files inventory" => "/missing/FILES",
            "files subtree" => "/missing/FILES/path",
            "file hash" => "/missing/file.txt/HASH",
            "undo stack" => "/missing/UNDO",
            "expiry inventory" => "/missing/EXPIRES",
            "expiry target" => "/missing/file.txt/EXPIRES",
            "immutable blob" => "/.blob/missing/deadbeef",
            _ => endpoint.path,
        };
        let response = send(&app, endpoint.method, path, &[]).await;
        assert_contract_status(endpoint.name, response.status());
        assert_eq!(response.headers()["symbol-api-version"], API_VERSION);
        assert_eq!(response.headers()["symbol-api-revision"], API_REVISION);
        assert_eq!(
            response.headers()["symbol-api-source-hash"],
            API_SOURCE_HASH
        );
    }
}

#[test]
fn emitted_application_logs_never_contain_request_or_response_secrets() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(CapturedLogs(Arc::clone(&captured)))
        .with_ansi(false)
        .without_time()
        .with_max_level(tracing::Level::TRACE)
        .finish();
    let dispatch = tracing::Dispatch::new(subscriber);
    let claim = CREATOR_CLAIM;
    let management = "sym_mgmt_0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    let request = build_request(
        "PUT",
        "/logged/index.html",
        &[
            ("management-action", "claim"),
            ("creator-claim", claim),
            ("idempotency-key", "never-log-this-key"),
        ],
        Body::empty(),
    );
    tracing::dispatcher::with_default(&dispatch, || {
        let span = make_http_span(&request);
        tracing::info!(parent: &span, "started processing request");
        tracing::info!(parent: &span, status = 201, "finished processing request");
    });
    let logs = captured_text(&captured);
    for secret in [claim, "never-log-this-key", management] {
        assert!(!logs.contains(secret));
    }
    assert!(logs.contains("started processing request"), "{logs}");
    assert!(logs.contains("finished processing request"), "{logs}");
}

#[tokio::test(flavor = "current_thread")]
async fn configured_info_logging_exposes_active_and_finished_mutations() {
    let captured = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::fmt()
        .with_writer(CapturedLogs(Arc::clone(&captured)))
        .with_ansi(false)
        .without_time()
        .with_env_filter("info,symbol::mutation=info")
        .finish();
    let dispatch = tracing::Dispatch::new(subscriber);
    let _default = tracing::dispatcher::set_default(&dispatch);
    log_mutation_signals_ready();
    let release = Arc::new(tokio::sync::Notify::new());
    let handler_release = Arc::clone(&release);
    let app = Router::new()
        .route(
            "/",
            axum::routing::put(move || {
                let release = Arc::clone(&handler_release);
                async move {
                    release.notified().await;
                    StatusCode::NO_CONTENT
                }
            }),
        )
        .layer(middleware::from_fn(log_mutation_activity));
    let request = tokio::spawn(app.oneshot(build_request("PUT", "/", &[], Body::empty())));

    for _ in 0..100 {
        if String::from_utf8_lossy(&captured.lock().unwrap()).contains("symbol_mutation_start") {
            break;
        }
        tokio::task::yield_now().await;
    }
    let active_logs = captured_text(&captured);
    assert!(active_logs.contains("symbol::mutation"), "{active_logs}");
    assert!(
        active_logs.contains("symbol_mutation_signals_ready"),
        "{active_logs}"
    );
    assert!(
        active_logs.contains("symbol_mutation_start"),
        "{active_logs}"
    );
    assert!(active_logs.contains("method=PUT"), "{active_logs}");
    assert!(active_logs.contains("mutation_id="), "{active_logs}");
    assert!(
        !active_logs.contains("symbol_mutation_finish"),
        "{active_logs}"
    );
    let start_id = mutation_id(&active_logs, "symbol_mutation_start");

    release.notify_one();
    assert_eq!(
        request.await.unwrap().unwrap().status(),
        StatusCode::NO_CONTENT
    );
    let finished_logs = captured_text(&captured);
    assert!(
        finished_logs.contains("symbol_mutation_finish"),
        "{finished_logs}"
    );
    assert!(finished_logs.contains("status=204"), "{finished_logs}");
    assert_eq!(
        mutation_id(&finished_logs, "symbol_mutation_finish"),
        start_id
    );
}

#[tokio::test]
async fn stats_response_keeps_original_fields_and_adds_distributions() {
    let (_root, store) = temp_store();
    store.put_file("one", "a.txt", b"same").unwrap();
    store.put_file("two", "b.txt", b"same").unwrap();
    let response = stats(State(test_app(store))).await.into_response();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    let json: serde_json::Value = body_json(response).await;
    assert_eq!(json["sites"], 2);
    assert_eq!(json["files"], 2);
    assert_eq!(json["aliases"], 0);
    assert_eq!(json["blobs"], 1);
    assert_eq!(json["bytes"], 4);
    assert_eq!(json["logical_bytes"], 8);
    assert_eq!(json["saved_bytes"], 4);
    assert_eq!(json["file_sizes"]["median"], 4.0);
    assert_eq!(json["blob_sizes"]["median"], 4.0);
    assert!(json["serving"]["readers"]["operations"].as_u64().is_some());
}

#[tokio::test]
async fn expiry_routes_return_reports_headers_and_inherited_deadlines() {
    let (_root, store) = temp_store();
    store.put_file("hello", "index.html", b"hello").unwrap();
    let app = app_router(&store);

    let response = send(
        &app,
        "EXPIRE",
        "/hello",
        &[("expiry-mode", "relative"), ("expiry-in", "1h")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["expiry-mode"], "relative");
    assert!(response.headers().contains_key(header::EXPIRES));
    assert!(response.headers().contains_key("undo-token"));
    let report: expiry::ExpiryReport = body_json(response).await;
    assert_eq!(report.target.kind, expiry::ExpiryTargetKind::Site);

    let response = get_with(&app, "/hello/EXPIRES", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    let report: expiry::ExpirySiteReport = body_json(response).await;
    assert_eq!(report.site, "hello");
    assert_eq!(report.entries.len(), 1);
    assert_eq!(
        report.entries[0].target.kind,
        expiry::ExpiryTargetKind::Site
    );

    let response = send(
        &app,
        "EXPIRE",
        "/hello/index.html",
        &[("expiry-mode", "never")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let report: expiry::ExpiryReport = body_json(response).await;
    assert!(report.own_policy.is_none());
    assert_eq!(
        report.limited_by.unwrap().kind,
        expiry::ExpiryTargetKind::Site
    );

    let response = get_with(&app, "/hello/index.html/EXPIRES", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-cache");

    let response = get_with(&app, "/hello/index.html", &[]).await;
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(response.headers()[header::LOCATION], "/hello/index");

    let response = get_with(&app, "/hello/index", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-cache");
    assert!(response.headers().contains_key(header::EXPIRES));
}

#[tokio::test(flavor = "current_thread")]
async fn saturated_store_tasks_do_not_block_the_async_runtime() {
    let root = tempfile::tempdir().unwrap();
    let app = test_app(Store::new(root.path().to_path_buf()).unwrap());
    let capacity = app.store.blocking_capacity();
    let release = Arc::new(std::sync::Barrier::new(capacity + 1));
    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut tasks = Vec::with_capacity(capacity);

    for _ in 0..capacity {
        let app = app.clone();
        let release = Arc::clone(&release);
        let started_tx = started_tx.clone();
        tasks.push(tokio::spawn(async move {
            app.run_store(move |_| {
                started_tx.send(()).unwrap();
                release.wait();
                Ok(())
            })
            .await
        }));
    }
    for _ in 0..capacity {
        started_rx.recv().await.unwrap();
    }

    let extra_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let extra = {
        let app = app.clone();
        let extra_ran = Arc::clone(&extra_ran);
        tokio::spawn(async move {
            app.run_store(move |_| {
                extra_ran.store(true, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            })
            .await
        })
    };
    let runtime_marker = tokio::spawn(async { 7_u8 });
    assert_eq!(runtime_marker.await.unwrap(), 7);
    assert!(!extra_ran.load(std::sync::atomic::Ordering::Relaxed));

    release.wait();
    for task in tasks {
        task.await.unwrap().unwrap();
    }
    extra.await.unwrap().unwrap();
    assert!(extra_ran.load(std::sync::atomic::Ordering::Relaxed));
}

#[tokio::test]
async fn directory_etag_changes_after_visible_publish() {
    let (_root, store) = temp_store();
    store.put_file("hello", "a.txt", b"a").unwrap();
    let app = test_app(store.clone());
    let response = browse_dir(
        &app,
        "hello",
        "",
        true,
        &HeaderMap::new(),
        browse::ListingView::default(),
    )
    .await
    .into_response();
    let etag = response.headers()[header::ETAG].clone();

    let mut conditional = HeaderMap::new();
    conditional.insert(header::IF_NONE_MATCH, etag.clone());
    assert_eq!(
        browse_dir(
            &app,
            "hello",
            "",
            true,
            &conditional,
            browse::ListingView::default(),
        )
        .await
        .into_response()
        .status(),
        StatusCode::NOT_MODIFIED
    );

    store.put_file("hello", "b.txt", b"bb").unwrap();
    let response = browse_dir(
        &app,
        "hello",
        "",
        true,
        &conditional,
        browse::ListingView::default(),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::OK);
    assert_ne!(response.headers()[header::ETAG], etag);
}

#[tokio::test]
async fn content_addressed_blob_is_immutable_and_site_scoped() {
    let (_root, store) = temp_store();
    store.put_file("hello", "asset.bin", b"asset").unwrap();
    store.put_file("other", "index.html", b"other").unwrap();
    let hash = file_hash(&store, "hello", "asset.bin");
    let app = test_app(store);

    let response = serve_immutable_blob(
        State(app.clone()),
        Path(("hello".to_string(), hash.to_wire())),
        HeaderMap::new(),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::OK);
    assert_header(
        &response,
        "cache-control",
        "public, max-age=31536000, immutable",
    );
    let etag = response.headers()[header::ETAG].clone();
    assert_eq!(body_bytes(response).await, b"asset");

    let mut conditional = HeaderMap::new();
    conditional.insert(header::IF_NONE_MATCH, etag);
    let response = serve_immutable_blob(
        State(app.clone()),
        Path(("hello".to_string(), hash.to_wire())),
        conditional,
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);

    let response = serve_immutable_blob(
        State(app),
        Path(("other".to_string(), hash.to_wire())),
        HeaderMap::new(),
    )
    .await
    .into_response();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn extensionless_get_falls_back_to_html_then_htm_without_shadowing_exact_files() {
    let (_root, store) = temp_store();
    put_text_files(
        &store,
        "hello",
        &[
            ("about.html", "html fallback"),
            ("legacy.htm", "htm fallback"),
            ("contact.html", "html fallback"),
            ("about", "exact file"),
            ("contact.htm", "htm sibling"),
        ],
    );
    let app = app_router(&store);

    for (path, expected) in [
        ("/hello/about", "exact file"),
        ("/hello/contact", "html fallback"),
        ("/hello/legacy", "htm fallback"),
    ] {
        let response = get_with(&app, path, &[]).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(body_bytes(response).await, expected.as_bytes(), "{path}");
    }

    let missing = get_with(&app, "/hello/missing", &[]).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);

    for (path, location) in [
        ("/hello/contact.html", "/hello/contact"),
        ("/hello/legacy.htm", "/hello/legacy"),
    ] {
        let response = get_with(&app, path, &[]).await;
        assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT, "{path}");
        assert_eq!(response.headers()[header::LOCATION], location, "{path}");
    }

    let shadowed = get_with(&app, "/hello/about.html", &[]).await;
    assert_eq!(shadowed.status(), StatusCode::OK);
    assert_eq!(body_bytes(shadowed).await, b"html fallback");

    let htm_sibling = get_with(&app, "/hello/contact.htm", &[]).await;
    assert_eq!(htm_sibling.status(), StatusCode::OK);
    assert_eq!(body_bytes(htm_sibling).await, b"htm sibling");

    let files = get_with(&app, "/hello/FILES/contact.html", &[]).await;
    assert_eq!(files.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(files.headers()[header::LOCATION], "/hello/contact");
}

#[tokio::test]
async fn immutable_blob_route_does_not_shadow_site_files() {
    let (_root, store) = temp_store();
    put_text_files(&store, "hello", &[(".blob/custom", "site file")]);
    let hash = file_hash(&store, "hello", ".blob/custom");
    let app = app_router(&store);

    let response = get_with(&app, "/hello/.blob/custom", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body_bytes(response).await, b"site file");

    let response = get_with(&app, &format!("/.blob/hello/{hash}"), &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn media_responses_support_ranges_seeking_and_head() {
    let (_root, store) = temp_store();
    store.put_file("media", "song.mp3", b"0123456789").unwrap();
    let app = app_router(&store);

    let response = get_with(&app, "/media/song.mp3", &[("range", "bytes=2-5")]).await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_header(&response, "content-type", "audio/mpeg");
    assert_header(&response, "content-length", "4");
    assert_header(&response, "content-range", "bytes 2-5/10");
    assert_header(&response, "accept-ranges", "bytes");
    assert_eq!(body_bytes(response).await, b"2345");

    let response = get_with(
        &app,
        "/media/song.mp3",
        &[("range", "bytes=2-5"), ("if-range", "\"stale\"")],
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_header(&response, "content-length", "10");
    assert_eq!(body_bytes(response).await, b"0123456789");

    let response = get_with(&app, "/media/song.mp3", &[("range", "bytes=99-")]).await;
    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_header(&response, "content-range", "bytes */10");

    let response = send(&app, "HEAD", "/media/song.mp3", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_header(&response, "content-length", "10");
    assert!(body_bytes(response).await.is_empty());
}

#[test]
fn range_parser_handles_open_suffix_and_if_range_requests() {
    let mut headers = HeaderMap::new();
    headers.insert(header::RANGE, HeaderValue::from_static("bytes=4-"));
    assert_eq!(
        requested_range(&headers, 10, "\"hash\"")
            .unwrap()
            .unwrap()
            .len(),
        6
    );
    headers.insert(header::RANGE, HeaderValue::from_static("bytes=-3"));
    let suffix = requested_range(&headers, 10, "\"hash\"").unwrap().unwrap();
    assert_eq!((suffix.start, suffix.end), (7, 9));
    headers.insert(header::IF_RANGE, HeaderValue::from_static("\"other\""));
    assert!(requested_range(&headers, 10, "\"hash\"").unwrap().is_none());
    headers.remove(header::IF_RANGE);
    headers.insert(header::RANGE, HeaderValue::from_static("bytes=0-1,4-5"));
    assert!(requested_range(&headers, 10, "\"hash\"").unwrap().is_none());
}

#[tokio::test]
async fn upload_limits_reject_content_length_and_chunked_overflow() {
    let (root, store) = temp_store();
    let app = router(App::with_max_file_size(store.clone(), 4));

    let response = send_body(
        &app,
        "PUT",
        "/media/declared.bin",
        &[("content-length", "5")],
        Body::from("12345"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let chunks = futures_util::stream::iter([
        Ok::<_, io::Error>(ByteChunk::from_static(b"123")),
        Ok(ByteChunk::from_static(b"456")),
    ]);
    let response = send_body(
        &app,
        "PUT",
        "/media/chunked.bin",
        &[],
        Body::from_stream(chunks),
    )
    .await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(store.stats().unwrap().files, 0);
    assert!(tmp_dir_is_empty(root.path()));
}

#[tokio::test]
async fn archive_download_and_pop_stream_then_remove_temporary_files() {
    let (root, store) = temp_store();
    store
        .put_file("media", "large.bin", &vec![9_u8; 2 * 1024 * 1024])
        .unwrap();
    let app = app_router(&store);

    let response = get_with(&app, "/media.tar", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    let length = response.headers()[header::CONTENT_LENGTH]
        .to_str()
        .unwrap()
        .parse::<usize>()
        .unwrap();
    let archive = body_bytes(response).await;
    assert_eq!(archive.len(), length);
    assert_eq!(&archive[257..262], b"ustar");
    assert!(store.site_exists("media"));
    assert!(tmp_dir_is_empty(root.path()));

    let response = send(&app, "DELETE", "/media", &[]).await;
    assert_eq!(response.status(), StatusCode::OK);
    let archive = body_bytes(response).await;
    assert_eq!(&archive[..2], [0x1f, 0x8b]);
    assert!(!store.site_exists("media"));
    assert!(tmp_dir_is_empty(root.path()));
}

#[tokio::test]
async fn delete_archives_use_the_same_tar_zip_and_gzip_formats_as_get() {
    let (_root, store) = temp_store();
    let app = app_router(&store);
    for (name, extension, signature) in [
        ("plain", ".tar", b"ustar".as_slice()),
        ("compressed", ".tar.gz", &[0x1f, 0x8b]),
        ("zipped", ".zip", b"PK\x03\x04".as_slice()),
    ] {
        store.put_file(name, "index.html", b"hello").unwrap();
        let response = send(&app, "DELETE", &format!("/{name}{extension}"), &[]).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().contains_key("undo-token"));
        let archive = body_bytes(response).await;
        if extension == ".tar" {
            assert_eq!(&archive[257..262], signature);
        } else {
            assert_eq!(&archive[..signature.len()], signature);
        }
        assert!(!store.site_exists(name));
    }
    for method in ["GET", "DELETE"] {
        let response = send(&app, method, "/unsupported.rar", &[]).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}

#[tokio::test]
async fn copy_and_move_handlers_follow_contract() {
    let (_root, store) = temp_store();
    store.put_file("source", "index.html", b"hello").unwrap();
    let app = app_router(&store);

    let copied = send(&app, "COPY", "/source", &[("destination", "/copied")]).await;
    assert_eq!(copied.status(), StatusCode::CREATED);
    assert_eq!(copied.headers()["location"], "http://symbol/copied/");
    assert!(copied.headers().contains_key("content-revision"));
    assert!(copied.headers().contains_key(header::ETAG));
    assert!(copied.headers().contains_key("undo-token"));

    let conflict = send(&app, "COPY", "/source", &[("destination", "/copied")]).await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);

    let moved = send(&app, "MOVE", "/copied", &[("destination", "/renamed")]).await;
    assert_eq!(moved.status(), StatusCode::OK);
    assert_eq!(moved.headers()["location"], "http://symbol/renamed/");
    assert!(!store.site_exists("copied"));
    assert!(store.site_exists("renamed"));
}

#[tokio::test]
async fn inventory_and_put_preconditions_follow_contract() {
    let (_root, store) = temp_store();
    store.put_file("source", "index.html", b"hello").unwrap();
    let app = app_router(&store);
    let inventory = get_with(&app, "/source/FILES", &[("accept", "application/json")]).await;
    assert_eq!(inventory.status(), StatusCode::OK);
    let baseline_etag = header_string(&inventory, "etag");
    let baseline_revision = header_string(&inventory, "content-revision");
    let json: serde_json::Value = body_json(inventory).await;
    assert_eq!(json["site"], "source");
    assert_eq!(
        json["content_revision"].as_u64().unwrap().to_string(),
        baseline_revision
    );
    assert_eq!(json["files"][0]["path"], "index.html");
    assert!(
        json["files"][0]["hash"]
            .as_str()
            .unwrap()
            .starts_with("blake3:")
    );

    let updated = send_body(
        &app,
        "PUT",
        "/source/index.html",
        &[("if-match", &baseline_etag)],
        Body::from("updated"),
    )
    .await;
    assert_eq!(updated.status(), StatusCode::OK);
    let current_revision = updated.headers()["content-revision"].clone();

    let drift = send_body(
        &app,
        "PUT",
        "/source/new.txt",
        &[("if-match", &baseline_etag)],
        Body::from("rejected"),
    )
    .await;
    assert_eq!(drift.status(), StatusCode::PRECONDITION_FAILED);
    assert_eq!(drift.headers()["content-revision"], current_revision);
    assert!(matches!(
        store.lookup("source", "new.txt"),
        Err(StoreError::NotFound)
    ));
}

#[tokio::test]
async fn replace_header_prunes_on_site_put_and_is_rejected_on_file_put() {
    let (_root, store) = temp_store();
    put_text_files(
        &store,
        "hello",
        &[("keep.txt", "keep"), ("drop.txt", "drop")],
    );
    let app = app_router(&store);

    let rejected = send_body(
        &app,
        "PUT",
        "/hello/keep.txt",
        &[("Replace", "1")],
        Body::from("nope"),
    )
    .await;
    assert_eq!(rejected.status(), StatusCode::BAD_REQUEST);
    assert!(store.lookup("hello", "drop.txt").is_ok());

    let unnamed = send_body(
        &app,
        "PUT",
        "/",
        &[("Replace", "1"), ("content-type", "text/html")],
        Body::from("<p>nope</p>"),
    )
    .await;
    assert_eq!(unnamed.status(), StatusCode::BAD_REQUEST);

    let replaced = send_body(
        &app,
        "PUT",
        "/hello",
        &[
            ("Replace", "1"),
            ("content-disposition", "attachment; filename=\"keep.txt\""),
        ],
        Body::from("kept"),
    )
    .await;
    assert_eq!(replaced.status(), StatusCode::OK);
    assert!(store.lookup("hello", "keep.txt").is_ok());
    assert!(matches!(
        store.lookup("hello", "drop.txt"),
        Err(StoreError::NotFound)
    ));
    assert!(store.lookup("hello", "symbol.toml").is_ok());
    let undo = header_string(&replaced, "undo-token");
    let restored = send(&app, "UNDO", "/hello", &[("Undo-Token", &undo)]).await;
    assert_eq!(restored.status(), StatusCode::OK);
    assert!(store.lookup("hello", "drop.txt").is_ok());
}

#[tokio::test]
async fn generated_resource_handlers_replay_idempotent_requests() {
    let (_root, store) = temp_store();
    store.put_file("source", "index.html", b"hello").unwrap();
    let app = &app_router(&store);
    let auto_copy = |key: &'static str| async move {
        send(app, "COPY", "/source", &[("idempotency-key", key)]).await
    };
    let first = auto_copy("copy-retry").await;
    let first_location = first.headers()["location"].clone();
    let replay = auto_copy("copy-retry").await;
    assert_eq!(replay.headers()["location"], first_location);

    let unnamed_put = |body: &'static str| async move {
        send_body(
            app,
            "PUT",
            "/",
            &[("idempotency-key", "put-retry")],
            Body::from(body),
        )
        .await
    };
    let first = unnamed_put("same").await;
    let first_location = first.headers()["location"].clone();
    let replay = unnamed_put("same").await;
    assert_eq!(replay.headers()["location"], first_location);
    let mismatch = unnamed_put("different").await;
    assert_eq!(mismatch.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn generated_managed_replay_never_returns_new_credentials() {
    let (_root, store) = temp_store();
    let app = app_router(&store);
    let request = || {
        send_body(
            &app,
            "PUT",
            "/",
            &[
                ("idempotency-key", "managed-put-retry"),
                ("management-action", "claim"),
                ("creator-claim", CREATOR_CLAIM),
            ],
            Body::from("same"),
        )
    };
    let first = request().await;
    assert_eq!(first.status(), StatusCode::CREATED);
    assert!(first.headers().contains_key("management-token"));
    let location = first.headers()["location"].clone();

    let replay = request().await;
    assert_eq!(replay.headers()["location"], location);
    assert_eq!(replay.headers()["idempotency-replayed"], "true");
    assert!(!replay.headers().contains_key("management-token"));
    assert!(!replay.headers().contains_key("creator-claim"));
}

#[tokio::test]
async fn large_chunked_upload_is_spooled_and_range_served() {
    let (_root, store) = temp_store();
    let app = app_router(&store);
    let chunk = ByteChunk::from(vec![7_u8; 1024 * 1024]);
    let stream =
        futures_util::stream::iter((0..51).map(move |_| Ok::<_, io::Error>(chunk.clone())));

    let response = send_body(
        &app,
        "PUT",
        "/media/song.mp3",
        &[],
        Body::from_stream(stream),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(store.stats().unwrap().bytes, 51 * 1024 * 1024);

    let response = get_with(&app, "/media/song.mp3", &[("range", "bytes=0-3")]).await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(body_bytes(response).await, [7_u8; 4]);
}

#[tokio::test]
async fn mutation_headers_stack_and_guarded_undo_follow_contract() {
    let (_root, store) = temp_store();
    let app = app_router(&store);

    let created = send_body(&app, "PUT", "/hello/index.html", &[], Body::from("first")).await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let first_token = header_string(&created, "undo-token");
    assert!(created.headers().contains_key("undo-expires"));
    assert_eq!(
        created.headers()["location"],
        "http://symbol/hello/index.html"
    );

    let updated = send_body(&app, "PUT", "/hello/other.txt", &[], Body::from("second")).await;
    assert_eq!(updated.status(), StatusCode::OK);
    let latest_token = header_string(&updated, "undo-token");

    let stack = get_with(&app, "/hello/UNDO", &[]).await;
    assert_eq!(stack.status(), StatusCode::OK);
    let json: serde_json::Value = body_json(stack).await;
    assert_eq!(json["entries"].as_array().unwrap().len(), 2);

    let stale = send(&app, "UNDO", "/hello", &[("undo-token", &first_token)]).await;
    assert_eq!(stale.status(), StatusCode::CONFLICT);

    let restored = send(&app, "UNDO", "/hello", &[("undo-token", &latest_token)]).await;
    assert_eq!(restored.status(), StatusCode::OK);
    assert!(matches!(
        store.lookup("hello", "other.txt"),
        Err(StoreError::NotFound)
    ));
    assert!(matches!(
        store.lookup("hello", "index.html"),
        Ok(store::Node::File { .. })
    ));
}

#[tokio::test]
async fn handler_file_delete_followed_by_guarded_undo_restores_the_file() {
    let (_root, store) = temp_store();
    put_text_files(&store, "hello", &[("remove.txt", "restore me")]);
    let app = app_router(&store);
    let deleted = send(&app, "DELETE", "/hello/remove.txt", &[]).await;
    assert_contract_status("file delete", deleted.status());
    let token = header_string(&deleted, "undo-token");
    let missing = get_with(&app, "/hello/remove.txt", &[]).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    let restored = send(&app, "UNDO", "/hello", &[("undo-token", &token)]).await;
    assert_contract_status("site undo", restored.status());
    let content = get_with(&app, "/hello/remove.txt", &[]).await;
    assert_eq!(content.status(), StatusCode::OK);
    assert_eq!(body_bytes(content).await, b"restore me");
}

#[tokio::test]
async fn managed_mutations_authorize_before_spooling_and_rotation_is_idempotent() {
    use std::sync::atomic::{AtomicBool, Ordering};

    let (_root, store) = temp_store();
    let app = app_router(&store);
    let created = send_body(
        &app,
        "PUT",
        "/secure/index.html",
        &[("management-action", "claim")],
        Body::from("initial"),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    assert_eq!(created.headers()[header::CACHE_CONTROL], "no-store");
    let token = header_string(&created, "management-token");
    let claim = header_string(&created, "creator-claim");

    let polled = Arc::new(AtomicBool::new(false));
    let polled_by_body = Arc::clone(&polled);
    let body = Body::from_stream(futures_util::stream::once(async move {
        polled_by_body.store(true, Ordering::SeqCst);
        Ok::<_, io::Error>(ByteChunk::from_static(b"must not spool"))
    }));
    let unauthorized = send_body(&app, "PUT", "/secure/rejected.txt", &[], body).await;
    assert_eq!(unauthorized.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        unauthorized.headers()[header::WWW_AUTHENTICATE],
        "Bearer realm=\"symbol\""
    );
    assert!(!polled.load(Ordering::SeqCst));

    let sanitized = send_body(
        &app,
        "PUT",
        "/secure/leak.txt",
        &[("authorization", &bearer(&token))],
        Body::from(token.clone()),
    )
    .await;
    assert_eq!(sanitized.status(), StatusCode::OK);
    assert_eq!(sanitized.headers()["sanitized-management-tokens"], "1");
    let hash = file_hash(&store, "secure", "leak.txt");
    let stored = store.read_blob(hash).unwrap();
    assert_eq!(stored.len(), token.len());
    assert!(stored.starts_with(secrets::MANAGEMENT_TOKEN_PREFIX.as_bytes()));
    assert!(!stored.windows(16).any(|window| {
        token
            .as_bytes()
            .windows(16)
            .any(|candidate| candidate == window)
    }));

    let rotate = || async {
        send(
            &app,
            "MANAGE",
            "/secure",
            &[
                ("management-action", "rotate"),
                ("idempotency-key", "rotation-1"),
                ("authorization", &bearer(&token)),
                ("creator-claim", &claim),
            ],
        )
        .await
    };
    let rotated = rotate().await;
    assert_eq!(rotated.status(), StatusCode::OK);
    let rotated_token = header_string(&rotated, "management-token");
    assert_ne!(rotated_token, token);

    let replay = rotate().await;
    assert_eq!(replay.status(), StatusCode::OK);
    assert_eq!(replay.headers()["idempotency-replayed"], "true");
    assert!(!replay.headers().contains_key("management-token"));

    let stale = send(
        &app,
        "DELETE",
        "/secure/leak.txt",
        &[("authorization", &bearer(&token))],
    )
    .await;
    assert_eq!(stale.status(), StatusCode::UNAUTHORIZED);

    let deleted = send(
        &app,
        "DELETE",
        "/secure/leak.txt",
        &[("authorization", &bearer(&rotated_token))],
    )
    .await;
    assert_eq!(deleted.status(), StatusCode::OK);
}

#[tokio::test]
async fn receipt_claim_copy_isolation_move_and_managed_delete_undo_follow_contract() {
    let (_root, store) = temp_store();
    let app = app_router(&store);
    let created = send_body(&app, "PUT", "/source/index.html", &[], Body::from("source")).await;
    let claim = header_string(&created, "creator-claim");
    let claimed = send(
        &app,
        "MANAGE",
        "/source",
        &[
            ("management-action", "claim"),
            ("creator-claim", &claim),
            ("idempotency-key", "claim-1"),
        ],
    )
    .await;
    assert_eq!(claimed.status(), StatusCode::OK);
    let token = header_string(&claimed, "management-token");

    let copied = send(&app, "COPY", "/source", &[("destination", "/public-copy")]).await;
    assert_eq!(copied.status(), StatusCode::CREATED);
    assert!(!copied.headers().contains_key("management-token"));
    assert!(copied.headers().contains_key("creator-claim"));

    let managed_copy = send(
        &app,
        "COPY",
        "/source",
        &[
            ("destination", "/managed-copy"),
            ("management-action", "claim"),
        ],
    )
    .await;
    let copy_token = header_string(&managed_copy, "management-token");
    assert_ne!(copy_token, token);

    let moved = send(
        &app,
        "MOVE",
        "/managed-copy",
        &[
            ("destination", "/moved-copy"),
            ("authorization", &bearer(&copy_token)),
        ],
    )
    .await;
    assert_eq!(moved.status(), StatusCode::OK);
    let authorize_moved_copy = || {
        store
            .authorize_mutation(
                "moved-copy",
                Some(&ManagementToken::parse(&copy_token).unwrap()),
            )
            .unwrap();
    };
    authorize_moved_copy();

    let deleted = send(
        &app,
        "DELETE",
        "/moved-copy",
        &[("authorization", &bearer(&copy_token))],
    )
    .await;
    let undo = header_string(&deleted, "undo-token");
    let rejected = send(&app, "UNDO", "/moved-copy", &[("undo-token", &undo)]).await;
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
    let restored = send(
        &app,
        "UNDO",
        "/moved-copy",
        &[
            ("undo-token", &undo),
            ("authorization", &bearer(&copy_token)),
        ],
    )
    .await;
    assert_eq!(restored.status(), StatusCode::OK);
    authorize_moved_copy();
}

#[tokio::test]
async fn trusted_proxy_identity_requires_an_allowlisted_socket_peer() {
    let (root, store) = temp_store();
    let provider = IdentityProvider::TrustedProxy {
        principal_header: HeaderName::from_static("x-authenticated-user"),
        peers: loopback_peers(),
    };
    let app = identity_router(store, provider);
    let created = send_from_peer(
        &app,
        ([127, 0, 0, 1], 12345),
        "PUT",
        "/principal/index.html",
        &[("x-authenticated-user", "user@example.test")],
        Body::from("principal"),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    assert!(!created.headers().contains_key("creator-claim"));

    let claimed = send_from_peer(
        &app,
        ([127, 0, 0, 1], 12346),
        "MANAGE",
        "/principal",
        &[
            ("management-action", "claim"),
            ("x-authenticated-user", "user@example.test"),
        ],
        Body::empty(),
    )
    .await;
    assert_eq!(claimed.status(), StatusCode::OK);
    assert!(claimed.headers().contains_key("management-token"));
    let audit_ip = latest_audit_source_ip(root.path(), "principal");
    assert_eq!(audit_ip.as_deref(), Some("127.0.0.1"));

    let forged = send_body(
        &app,
        "PUT",
        "/untrusted/index.html",
        &[
            ("x-authenticated-user", "user@example.test"),
            (INTERNAL_CREATOR_HEADER, "forged"),
        ],
        Body::from("untrusted"),
    )
    .await;
    assert!(forged.headers().contains_key("creator-claim"));
}

#[tokio::test]
async fn mtls_and_tailscale_principals_are_accepted_only_from_trusted_peers() {
    for (provider, header, site) in [
        (
            IdentityProvider::Mtls {
                principal_header: HeaderName::from_static("x-client-cert-sha256"),
                peers: loopback_peers(),
            },
            "x-client-cert-sha256",
            "mtls-site",
        ),
        (
            IdentityProvider::Tailscale {
                principal_header: HeaderName::from_static("tailscale-user-login"),
                peers: loopback_peers(),
            },
            "tailscale-user-login",
            "tailscale-site",
        ),
    ] {
        let (_root, store) = temp_store();
        let app = identity_router(store, provider);
        let response = send_from_peer(
            &app,
            ([127, 0, 0, 1], 12345),
            "PUT",
            &format!("/{site}/index.html"),
            &[(header, "stable-principal")],
            Body::from("content"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        assert!(!response.headers().contains_key("creator-claim"));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn tailscale_local_identity_is_resolved_through_configured_whois_command() {
    use std::os::unix::fs::PermissionsExt as _;

    let root = tempfile::tempdir().unwrap();
    let command = root.path().join("tailscale-whois");
    std::fs::write(
        &command,
        "#!/bin/sh\nprintf '%s\\n' '{\"UserProfile\":{\"LoginName\":\"user@example.test\"}}'\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&command).unwrap().permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&command, permissions).unwrap();
    let store = Store::new(root.path().join("data")).unwrap();
    let app = identity_router(
        store,
        IdentityProvider::TailscaleLocal {
            command: Arc::from(command),
        },
    );
    let response = send_from_peer(
        &app,
        ([100, 64, 0, 7], 12345),
        "PUT",
        "/tailscale-local/index.html",
        &[],
        Body::from("content"),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert!(!response.headers().contains_key("creator-claim"));
}

#[tokio::test]
async fn audit_ip_uses_forwarded_client_only_from_a_separately_trusted_proxy() {
    for (site, peer, forwarded, trusted, expected) in [
        (
            "trusted-audit",
            [127, 0, 0, 1],
            "203.0.113.9",
            true,
            "203.0.113.9",
        ),
        (
            "spoofed-audit",
            [10, 0, 0, 8],
            "203.0.113.10",
            false,
            "10.0.0.8",
        ),
    ] {
        let (root, store) = temp_store();
        let mut state = test_app(store);
        if trusted {
            state.audit_trusted_proxy = loopback_peers();
        }
        let app = router(state);
        let created = send_body(
            &app,
            "PUT",
            &format!("/{site}/index.html"),
            &[],
            Body::from("content"),
        )
        .await;
        let claim = header_string(&created, "creator-claim");
        let response = send_from_peer(
            &app,
            (peer, 12345),
            "MANAGE",
            &format!("/{site}"),
            &[
                ("management-action", "claim"),
                ("creator-claim", &claim),
                ("x-forwarded-for", forwarded),
            ],
            Body::empty(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let source_ip = latest_audit_source_ip(root.path(), site);
        assert_eq!(source_ip.as_deref(), Some(expected));
    }
}

#[tokio::test]
async fn api_virtual_site_redirects_negotiates_and_aliases_identically() {
    let (_root, store) = temp_store();
    let app = app_router(&store);

    let redirect = get_with(&app, "/API", &[]).await;
    assert_eq!(redirect.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(redirect.headers()[header::LOCATION], "/API/");

    let index = get_with(&app, "/API/", &[("accept", "text/html")]).await;
    assert_eq!(index.status(), StatusCode::OK);
    assert_header(&index, "content-type", "text/html; charset=utf-8");
    assert!(contains_bytes(
        &body_bytes(index).await,
        b"<h1>Symbol API</h1>"
    ));

    let canonical = get_with(&app, "/API/JS", &[("accept", "text/markdown")]).await;
    assert_eq!(canonical.status(), StatusCode::OK);
    assert_header(&canonical, "content-type", "text/markdown; charset=utf-8");
    let etag = canonical.headers()[header::ETAG].clone();
    let canonical_body = body_bytes(canonical).await;

    let alias = get_with(&app, "/API/TS", &[("accept", "text/markdown")]).await;
    assert_eq!(alias.status(), StatusCode::OK);
    assert_eq!(alias.headers()[header::ETAG], etag);
    assert_eq!(body_bytes(alias).await, canonical_body);
}

#[tokio::test]
async fn api_virtual_site_rejects_mutations_and_is_listed_as_builtin() {
    let (_root, store) = temp_store();
    let app = app_router(&store);

    for method in [
        "PUT", "POST", "DELETE", "COPY", "MOVE", "PATCH", "ALIAS", "REPLACE", "EXPIRE", "UNDO",
        "MANAGE",
    ] {
        let response = send(&app, method, "/API/JS", &[]).await;
        assert_eq!(
            response.status(),
            StatusCode::METHOD_NOT_ALLOWED,
            "{method}"
        );
        assert_eq!(response.headers()[header::ALLOW], "GET, HEAD");
    }

    let listing = get_with(&app, "/FILES", &[]).await;
    assert!(contains_bytes(&body_bytes(listing).await, b"API/"));
}

#[tokio::test]
async fn generated_sdk_assets_and_hashes_are_served_from_the_binary() {
    let (_root, store) = temp_store();
    let app = app_router(&store);

    for (path, content_type) in [
        ("/symbol.ts", "text/typescript; charset=utf-8"),
        ("/symbol.js", "text/javascript; charset=utf-8"),
        ("/symbol.global.js", "text/javascript; charset=utf-8"),
        ("/symbol.d.ts", "text/typescript; charset=utf-8"),
        ("/symbol.py", "text/x-python; charset=utf-8"),
    ] {
        let response = get_with(&app, path, &[]).await;
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(response.headers()[header::CONTENT_TYPE], content_type);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-cache");

        let hash = get_with(&app, &format!("{path}/HASH"), &[]).await;
        assert_eq!(hash.status(), StatusCode::OK, "{path}/HASH");
        assert_eq!(body_bytes(hash).await.len(), 65);
    }

    for (canonical, legacy) in [
        ("/symbol.ts", "/api.ts"),
        ("/symbol.js", "/api.js"),
        ("/symbol.global.js", "/api.global.js"),
        ("/symbol.d.ts", "/api.d.ts"),
        ("/symbol.py", "/api.py"),
    ] {
        let canonical = get_with(&app, canonical, &[]).await;
        let legacy = get_with(&app, legacy, &[]).await;
        assert_eq!(
            canonical.headers()[header::ETAG],
            legacy.headers()[header::ETAG]
        );
        assert_eq!(body_bytes(canonical).await, body_bytes(legacy).await);
    }

    let version = get_with(&app, "/API/VERSION", &[]).await;
    assert_eq!(version.status(), StatusCode::OK);
    let version: serde_json::Value = body_json(version).await;
    assert_eq!(version["api_version"], API_VERSION);
    assert_eq!(version["absolute_revision"].to_string(), API_REVISION);
    assert_eq!(version["source_hash"], API_SOURCE_HASH);
    assert_eq!(version["commit"], API_COMMIT);
    assert_eq!(version["dirty"], API_DIRTY == "true");
}
