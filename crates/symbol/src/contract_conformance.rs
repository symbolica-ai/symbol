use super::*;
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request};
use tower::ServiceExt as _;

use Fixture::{Empty, Managed, Site, Undo};

#[derive(Clone, Copy)]
enum Fixture {
    Empty,
    Site,
    Undo,
    Managed,
}

#[derive(Clone, Copy)]
enum Target {
    Literal(&'static str),
    Blob,
    /// A path under the current render-asset bundle, whose digest is only
    /// known at runtime.
    RenderAsset(&'static str),
}

#[derive(Clone, Copy)]
struct HeaderExpectation {
    name: &'static str,
    value: Option<&'static str>,
}

#[derive(Clone, Copy)]
struct Probe {
    endpoint: &'static str,
    fixture: Fixture,
    method: &'static str,
    target: Target,
    request_headers: &'static [(&'static str, &'static str)],
    body: &'static [u8],
    status: u16,
    response_headers: &'static [HeaderExpectation],
}

impl Probe {
    /// A body-less probe with no request headers; add them with
    /// [`Probe::headers`] and [`Probe::body`].
    const fn new(
        endpoint: &'static str,
        fixture: Fixture,
        method: &'static str,
        target: Target,
        status: u16,
        response_headers: &'static [HeaderExpectation],
    ) -> Self {
        Self {
            endpoint,
            fixture,
            method,
            target,
            request_headers: NO_REQUEST_HEADERS,
            body: b"",
            status,
            response_headers,
        }
    }

    const fn headers(self, request_headers: &'static [(&'static str, &'static str)]) -> Self {
        Self {
            request_headers,
            ..self
        }
    }

    const fn body(self, body: &'static [u8]) -> Self {
        Self { body, ..self }
    }
}

const fn at(path: &'static str) -> Target {
    Target::Literal(path)
}

/// A `GET` probe of a literal path.
const fn get(
    endpoint: &'static str,
    fixture: Fixture,
    path: &'static str,
    status: u16,
    response_headers: &'static [HeaderExpectation],
) -> Probe {
    Probe::new(endpoint, fixture, "GET", at(path), status, response_headers)
}

const fn header(name: &'static str) -> HeaderExpectation {
    HeaderExpectation { name, value: None }
}

const fn header_value(name: &'static str, value: &'static str) -> HeaderExpectation {
    HeaderExpectation {
        name,
        value: Some(value),
    }
}

const READ_HEADERS: &[HeaderExpectation] = &[
    header("content-type"),
    header("content-length"),
    header("etag"),
    header("cache-control"),
];
const MUTATION_HEADERS: &[HeaderExpectation] = &[
    header("location"),
    header("etag"),
    header("content-revision"),
    header("undo-token"),
    header("undo-expires"),
];
const ARCHIVE_HEADERS: &[HeaderExpectation] = &[
    header("content-type"),
    header("content-length"),
    header("content-disposition"),
    header_value("cache-control", "no-cache"),
];
const PLAIN_HEADER: &[HeaderExpectation] =
    &[header_value("content-type", "text/plain; charset=utf-8")];
const JSON_HEADER: &[HeaderExpectation] = &[header_value("content-type", "application/json")];
const CACHE_JSON_HEADERS: &[HeaderExpectation] = &[
    header_value("content-type", "application/json"),
    header_value("cache-control", "no-cache"),
];
/// A well-formed tree hash that no fixture site ever has.
const UNRELATED_TREE_HASH: &str =
    "blake3:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const NO_REQUEST_HEADERS: &[(&str, &str)] = &[];
const ETAG_NO_CACHE_HEADERS: &[HeaderExpectation] = &[
    header("content-type"),
    header("etag"),
    header_value("cache-control", "no-cache"),
];
const POP_HEADERS: &[HeaderExpectation] = &[
    header("content-type"),
    header("content-length"),
    header("content-disposition"),
    header("undo-token"),
    header("undo-expires"),
];
const EXPIRE_HEADERS: &[HeaderExpectation] = &[
    header("content-type"),
    header_value("cache-control", "no-cache"),
    header("expires"),
    header_value("expiry-mode", "relative"),
    header("undo-token"),
    header("undo-expires"),
];
const UNAUTHORIZED_HEADER: &[HeaderExpectation] =
    &[header_value("www-authenticate", "Bearer realm=\"symbol\"")];
const UNSATISFIABLE_RANGE_HEADERS: &[HeaderExpectation] = &[
    header("content-range"),
    header("accept-ranges"),
    header("etag"),
    header("cache-control"),
];

const SUCCESS_PROBES: &[Probe] = &[
    get("docs", Empty, "/", 200, ETAG_NO_CACHE_HEADERS),
    Probe::new("unnamed put", Empty, "PUT", at("/"), 201, MUTATION_HEADERS)
        .headers(&[("content-type", "text/html")])
        .body(b"<h1>unnamed</h1>"),
    get("docs hash", Empty, "/HASH", 200, PLAIN_HEADER),
    get("stats", Empty, "/STATS", 200, JSON_HEADER),
    get(
        "installer",
        Empty,
        "/install.sh",
        200,
        ETAG_NO_CACHE_HEADERS,
    ),
    get(
        "installer hash",
        Empty,
        "/install.sh/HASH",
        200,
        PLAIN_HEADER,
    ),
    get("client", Empty, "/symbol.sh", 200, ETAG_NO_CACHE_HEADERS),
    get("client hash", Empty, "/symbol.sh/HASH", 200, PLAIN_HEADER),
    get(
        "api documentation",
        Empty,
        "/API/JS",
        200,
        &[
            header_value("content-type", "text/markdown; charset=utf-8"),
            header("etag"),
            header_value("cache-control", "no-cache"),
            header_value("vary", "Accept, User-Agent"),
            header_value("link", "</API/JS>; rel=\"canonical\""),
        ],
    )
    .headers(&[("accept", "text/markdown")]),
    get(
        "api client asset",
        Empty,
        "/symbol.js",
        200,
        &[
            header_value("content-type", "text/javascript; charset=utf-8"),
            header("etag"),
            header_value("cache-control", "no-cache"),
        ],
    ),
    get(
        "api client hash",
        Empty,
        "/symbol.js/HASH",
        200,
        PLAIN_HEADER,
    ),
    get(
        "api version",
        Empty,
        "/API/VERSION",
        200,
        &[
            header_value("content-type", "application/json; charset=utf-8"),
            header("etag"),
            header_value("cache-control", "no-cache"),
        ],
    ),
    get("site listing", Site, "/FILES", 200, ETAG_NO_CACHE_HEADERS)
        .headers(&[("accept", "application/json")]),
    get(
        "site redirect",
        Site,
        "/hello",
        307,
        &[header_value("location", "/hello/")],
    ),
    get("site index", Site, "/hello/", 200, READ_HEADERS),
    Probe::new("site put", Site, "PUT", at("/hello"), 200, MUTATION_HEADERS)
        .headers(&[("content-type", "text/html")])
        .body(b"<h1>updated</h1>"),
    Probe::new("site pop", Site, "DELETE", at("/hello"), 200, POP_HEADERS),
    Probe::new(
        "site copy",
        Site,
        "COPY",
        at("/hello"),
        201,
        MUTATION_HEADERS,
    )
    .headers(&[("destination", "/copy")]),
    Probe::new(
        "site move",
        Site,
        "MOVE",
        at("/hello"),
        200,
        MUTATION_HEADERS,
    )
    .headers(&[("destination", "/moved")]),
    Probe::new("site undo", Undo, "UNDO", at("/hello"), 200, PLAIN_HEADER),
    Probe::new(
        "site expire",
        Site,
        "EXPIRE",
        at("/hello"),
        200,
        EXPIRE_HEADERS,
    )
    .headers(&[("expiry-mode", "relative"), ("expiry-in", "1h")]),
    Probe::new(
        "site management",
        Site,
        "MANAGE",
        at("/hello"),
        200,
        &[
            header("content-type"),
            header_value("cache-control", "no-store"),
        ],
    )
    .headers(&[("management-action", "status")]),
    get(
        "site file",
        Site,
        "/hello/assets/app.js",
        206,
        &[
            header("content-type"),
            header("content-length"),
            header("content-range"),
            header("accept-ranges"),
            header("etag"),
            header("cache-control"),
        ],
    )
    .headers(&[("range", "bytes=0-1")]),
    Probe::new(
        "file put",
        Site,
        "PUT",
        at("/hello/new.txt"),
        200,
        MUTATION_HEADERS,
    )
    .headers(&[("content-type", "text/plain")])
    .body(b"new"),
    Probe::new(
        "file delete",
        Site,
        "DELETE",
        at("/hello/assets/app.js"),
        200,
        &[
            header("content-type"),
            header("undo-token"),
            header("undo-expires"),
        ],
    ),
    Probe::new(
        "file expire",
        Site,
        "EXPIRE",
        at("/hello/assets/app.js"),
        200,
        EXPIRE_HEADERS,
    )
    .headers(&[("expiry-mode", "relative"), ("expiry-in", "1h")]),
    get("archive get", Site, "/hello.tar.gz", 200, ARCHIVE_HEADERS),
    Probe::new(
        "archive pop",
        Site,
        "DELETE",
        at("/hello.tar.gz"),
        200,
        POP_HEADERS,
    ),
    get(
        "files inventory",
        Site,
        "/hello/FILES",
        200,
        &[
            header_value("content-type", "application/json"),
            header("etag"),
            header("content-revision"),
            header_value("cache-control", "no-cache"),
        ],
    )
    .headers(&[("accept", "application/json")]),
    get(
        "files subtree",
        Site,
        "/hello/FILES/assets/",
        200,
        ETAG_NO_CACHE_HEADERS,
    )
    .headers(&[("accept", "application/json")]),
    get(
        "file hash",
        Site,
        "/hello/assets/app.js/HASH",
        200,
        PLAIN_HEADER,
    ),
    Probe::new(
        "render asset",
        Empty,
        "GET",
        Target::RenderAsset("markdown.css"),
        200,
        &[
            header_value("content-type", "text/css; charset=utf-8"),
            header("etag"),
            header_value("cache-control", "public, max-age=31536000, immutable"),
        ],
    ),
    get(
        "file raw",
        Site,
        "/hello/assets/app.js/RAW",
        200,
        &[
            header_value("content-type", "text/javascript; charset=utf-8"),
            header("content-length"),
            header("etag"),
            header("cache-control"),
            header("accept-ranges"),
        ],
    ),
    get("undo stack", Undo, "/hello/UNDO", 200, CACHE_JSON_HEADERS),
    get(
        "expiry inventory",
        Site,
        "/hello/EXPIRES",
        200,
        CACHE_JSON_HEADERS,
    ),
    get(
        "expiry target",
        Site,
        "/hello/assets/app.js/EXPIRES",
        200,
        CACHE_JSON_HEADERS,
    ),
    Probe::new(
        "immutable blob",
        Site,
        "GET",
        Target::Blob,
        206,
        &[
            header("content-type"),
            header("content-length"),
            header("content-range"),
            header("accept-ranges"),
            header("etag"),
            header_value("cache-control", "public, max-age=31536000, immutable"),
        ],
    )
    .headers(&[("range", "bytes=0-1")]),
    Probe::new(
        "alias batch",
        Site,
        "ALIAS",
        at("/hello/"),
        201,
        MUTATION_HEADERS,
    )
    .headers(&[("content-type", "application/json")])
    .body(br#"{"aliases":[{"path":"batch-link","target":"assets/app.js"}]}"#),
    Probe::new(
        "alias file",
        Site,
        "ALIAS",
        at("/hello/latest.js"),
        201,
        MUTATION_HEADERS,
    )
    .headers(&[("alias-target", "assets/app.js")]),
    Probe::new(
        "allocated file",
        Site,
        "POST",
        at("/hello/generated/"),
        201,
        &[
            header("content-type"),
            header("content-location"),
            header("location"),
            header("etag"),
            header("content-revision"),
            header("undo-token"),
            header("undo-expires"),
        ],
    )
    .headers(&[
        ("content-type", "application/octet-stream"),
        ("file-extension", "bin"),
    ])
    .body(b"allocated"),
    Probe::new(
        "file replace",
        Site,
        "REPLACE",
        at("/hello/assets/app.js"),
        200,
        MUTATION_HEADERS,
    )
    .headers(&[("if-content-match", "{fixture-file-hash}")])
    .body(b"replacement"),
    Probe::new(
        "file splice",
        Site,
        "PATCH",
        at("/hello/assets/app.js"),
        200,
        MUTATION_HEADERS,
    )
    .headers(&[
        ("if-content-match", "{fixture-file-hash}"),
        ("splice", "offset=0; delete=0; insert=1"),
    ])
    .body(b"X"),
];

const ERROR_PROBES: &[Probe] = &[
    Probe::new("unnamed put", Empty, "PUT", at("/"), 400, PLAIN_HEADER),
    get("site redirect", Empty, "/missing", 404, PLAIN_HEADER),
    get("site index", Empty, "/missing/", 404, PLAIN_HEADER),
    Probe::new("site put", Site, "PUT", at("/hello"), 400, PLAIN_HEADER),
    Probe::new(
        "site pop",
        Empty,
        "DELETE",
        at("/missing"),
        404,
        PLAIN_HEADER,
    ),
    Probe::new("site copy", Site, "COPY", at("/hello"), 409, PLAIN_HEADER)
        .headers(&[("destination", "/hello")]),
    Probe::new("site move", Site, "MOVE", at("/hello"), 400, PLAIN_HEADER),
    Probe::new(
        "site undo",
        Empty,
        "UNDO",
        at("/missing"),
        404,
        PLAIN_HEADER,
    ),
    Probe::new(
        "site expire",
        Site,
        "EXPIRE",
        at("/hello"),
        400,
        PLAIN_HEADER,
    )
    .headers(&[("expiry-mode", "relative")]),
    Probe::new(
        "site management",
        Site,
        "MANAGE",
        at("/hello"),
        400,
        PLAIN_HEADER,
    ),
    Probe::new(
        "site management",
        Site,
        "MANAGE",
        at("/hello"),
        403,
        PLAIN_HEADER,
    )
    .headers(&[("management-action", "claim")]),
    get(
        "site file",
        Site,
        "/hello/assets/app.js",
        416,
        UNSATISFIABLE_RANGE_HEADERS,
    )
    .headers(&[("range", "bytes=99-100")]),
    Probe::new(
        "file put",
        Site,
        "PUT",
        at("/hello/new.txt"),
        400,
        PLAIN_HEADER,
    ),
    Probe::new(
        "file delete",
        Site,
        "DELETE",
        at("/hello/missing.txt"),
        404,
        PLAIN_HEADER,
    ),
    Probe::new(
        "file expire",
        Site,
        "EXPIRE",
        at("/hello/missing.txt"),
        404,
        PLAIN_HEADER,
    ),
    get("archive get", Empty, "/missing.tar.gz", 404, PLAIN_HEADER),
    Probe::new(
        "archive pop",
        Empty,
        "DELETE",
        at("/missing.tar.gz"),
        404,
        PLAIN_HEADER,
    ),
    get(
        "files inventory",
        Empty,
        "/missing/FILES",
        404,
        PLAIN_HEADER,
    )
    .headers(&[("accept", "application/json")]),
    get(
        "files subtree",
        Empty,
        "/missing/FILES/path",
        404,
        PLAIN_HEADER,
    ),
    get(
        "file hash",
        Site,
        "/hello/missing.txt/HASH",
        404,
        PLAIN_HEADER,
    ),
    // A bundle this binary does not serve is a 404, never a substitution:
    // anything else would break the `immutable` promise.
    get(
        "render asset",
        Empty,
        "/ASSETS/0000000000000000/markdown.css",
        404,
        PLAIN_HEADER,
    ),
    Probe::new(
        "render asset",
        Empty,
        "GET",
        Target::RenderAsset("missing.css"),
        404,
        PLAIN_HEADER,
    ),
    get(
        "file raw",
        Site,
        "/hello/missing.txt/RAW",
        404,
        PLAIN_HEADER,
    ),
    // A directory has no raw bytes, even one with an index file.
    get("file raw", Site, "/hello/assets/RAW", 404, PLAIN_HEADER),
    get("file raw", Site, "/hello/RAW", 404, PLAIN_HEADER),
    get(
        "file raw",
        Site,
        "/hello/assets/app.js/RAW",
        416,
        UNSATISFIABLE_RANGE_HEADERS,
    )
    .headers(&[("range", "bytes=99-100")]),
    get(
        "expiry inventory",
        Empty,
        "/missing/EXPIRES",
        404,
        PLAIN_HEADER,
    ),
    get(
        "expiry target",
        Site,
        "/hello/missing.txt/EXPIRES",
        404,
        PLAIN_HEADER,
    ),
    get(
        "immutable blob",
        Site,
        "/.blob/hello/deadbeef",
        404,
        PLAIN_HEADER,
    ),
    Probe::new(
        "site put",
        Managed,
        "PUT",
        at("/hello"),
        401,
        &[
            header_value("content-type", "text/plain; charset=utf-8"),
            header_value("www-authenticate", "Bearer realm=\"symbol\""),
            header_value("cache-control", "no-store"),
        ],
    )
    .body(b"<h1>unauthorized</h1>"),
    Probe::new(
        "site pop",
        Managed,
        "DELETE",
        at("/hello"),
        401,
        &[
            header_value("www-authenticate", "Bearer realm=\"symbol\""),
            header_value("cache-control", "no-store"),
        ],
    ),
    Probe::new(
        "site move",
        Managed,
        "MOVE",
        at("/hello"),
        401,
        UNAUTHORIZED_HEADER,
    )
    .headers(&[("destination", "/moved")]),
    Probe::new(
        "site undo",
        Managed,
        "UNDO",
        at("/hello"),
        401,
        UNAUTHORIZED_HEADER,
    ),
    Probe::new(
        "site expire",
        Managed,
        "EXPIRE",
        at("/hello"),
        401,
        UNAUTHORIZED_HEADER,
    ),
    Probe::new(
        "file put",
        Managed,
        "PUT",
        at("/hello/new.txt"),
        401,
        UNAUTHORIZED_HEADER,
    )
    .body(b"unauthorized"),
    Probe::new(
        "file delete",
        Managed,
        "DELETE",
        at("/hello/assets/app.js"),
        401,
        UNAUTHORIZED_HEADER,
    ),
    Probe::new(
        "file expire",
        Managed,
        "EXPIRE",
        at("/hello/assets/app.js"),
        401,
        UNAUTHORIZED_HEADER,
    ),
    Probe::new(
        "archive pop",
        Managed,
        "DELETE",
        at("/hello.tar.gz"),
        401,
        UNAUTHORIZED_HEADER,
    ),
    Probe::new(
        "alias batch",
        Site,
        "ALIAS",
        at("/hello/"),
        400,
        PLAIN_HEADER,
    )
    .headers(&[("content-type", "application/json")])
    .body(br#"{"aliases":[]}"#),
    Probe::new(
        "alias file",
        Site,
        "ALIAS",
        at("/hello/latest.js"),
        400,
        PLAIN_HEADER,
    ),
    Probe::new(
        "allocated file",
        Empty,
        "POST",
        at("/missing/"),
        404,
        PLAIN_HEADER,
    )
    .body(b"must not spool"),
    Probe::new(
        "file replace",
        Site,
        "REPLACE",
        at("/hello/assets/app.js"),
        400,
        PLAIN_HEADER,
    )
    .body(b"replacement"),
    Probe::new(
        "file splice",
        Site,
        "PATCH",
        at("/hello/assets/app.js"),
        416,
        PLAIN_HEADER,
    )
    .headers(&[
        ("if-content-match", "{fixture-file-hash}"),
        ("splice", "offset=999; delete=0; insert=0"),
    ]),
    Probe::new(
        "alias batch",
        Managed,
        "ALIAS",
        at("/hello/"),
        401,
        UNAUTHORIZED_HEADER,
    )
    .headers(&[("content-type", "application/json")])
    .body(br#"{"aliases":[{"path":"blocked","target":"index.html"}]}"#),
    Probe::new(
        "alias file",
        Managed,
        "ALIAS",
        at("/hello/blocked"),
        401,
        UNAUTHORIZED_HEADER,
    )
    .headers(&[("alias-target", "index.html")]),
    Probe::new(
        "allocated file",
        Managed,
        "POST",
        at("/hello/"),
        401,
        UNAUTHORIZED_HEADER,
    )
    .body(b"must not spool"),
    Probe::new(
        "file replace",
        Managed,
        "REPLACE",
        at("/hello/index.html"),
        401,
        UNAUTHORIZED_HEADER,
    )
    .headers(&[("if-content-match", "{fixture-file-hash}")])
    .body(b"must not spool"),
    Probe::new(
        "file splice",
        Managed,
        "PATCH",
        at("/hello/index.html"),
        401,
        UNAUTHORIZED_HEADER,
    )
    .headers(&[
        ("if-content-match", "{fixture-file-hash}"),
        ("splice", "offset=0; delete=0; insert=1"),
    ])
    .body(b"x"),
];

fn fresh_store() -> (tempfile::TempDir, Store) {
    let root = tempfile::tempdir().unwrap();
    let store = Store::new(root.path().to_path_buf()).unwrap();
    (root, store)
}

fn put(store: &Store, site: &str, path: &str, bytes: &[u8]) {
    store.put_file(site, path, bytes).unwrap();
}

fn file_hash(store: &Store, site: &str, path: &str) -> ContentHash {
    let store::Node::File { hash, .. } = store.lookup(site, path).unwrap() else {
        panic!("{site}/{path} must be a file");
    };
    hash
}

fn set_relative_expiry(store: &Store, site: &str, path: &str, duration_seconds: u64) {
    store
        .set_expiry(
            site,
            path,
            Some(expiry::ExpiryPolicy::Relative { duration_seconds }),
        )
        .unwrap();
}

fn own_expiry_mode(store: &Store, site: &str, path: &str) -> expiry::ExpiryMode {
    store
        .expiry_report(site, path)
        .unwrap()
        .own_policy
        .unwrap()
        .mode
}

/// Spooled upload bodies live under `<root>/tmp`; a request rejected before
/// spooling leaves it empty.
fn tmp_entries(root: &std::path::Path) -> usize {
    std::fs::read_dir(root.join("tmp")).unwrap().count()
}

fn assert_replayed(response: &Response) {
    assert_eq!(response.headers()["idempotency-replayed"], "true");
}

fn header_text(response: &Response, name: impl axum::http::header::AsHeaderName) -> String {
    response.headers()[name].to_str().unwrap().to_string()
}

async fn body_bytes(response: Response) -> axum::body::Bytes {
    to_bytes(response.into_body(), usize::MAX).await.unwrap()
}

fn endpoint(name: &str) -> &'static contract::EndpointContract {
    contract::ENDPOINTS
        .iter()
        .find(|endpoint| endpoint.name == name)
        .unwrap_or_else(|| panic!("missing typed contract endpoint {name}"))
}

async fn fixture(kind: Fixture) -> (tempfile::TempDir, Store, Router) {
    let (root, store) = fresh_store();
    if matches!(kind, Fixture::Site | Fixture::Undo | Fixture::Managed) {
        put(&store, "hello", "index.html", b"<h1>hello</h1>");
        put(&store, "hello", "assets/app.js", b"console.log('hello')");
    }
    if matches!(kind, Fixture::Managed) {
        store.operator_claim("hello").unwrap();
    }
    let app = router(App::new(store.clone()));
    if matches!(kind, Fixture::Undo) {
        let response = send(&app, "PUT", "/hello/undo.txt", &[], "undo me").await;
        assert_eq!(response.status(), StatusCode::OK);
    }
    (root, store, app)
}

fn target_uri(target: Target, store: &Store) -> String {
    match target {
        Target::Literal(path) => path.to_string(),
        Target::Blob => {
            let hash = file_hash(store, "hello", "assets/app.js");
            format!("/.blob/hello/{}", hash.to_hex())
        }
        Target::RenderAsset(path) => format!("{}/{path}", crate::assets::base()),
    }
}

async fn execute(probe: Probe) {
    let (_root, store, app) = fixture(probe.fixture).await;
    let contract = endpoint(probe.endpoint);
    assert_eq!(probe.method, contract.method, "{} method", probe.endpoint);
    let fixture_file_hash = match store.lookup("hello", "assets/app.js") {
        Ok(store::Node::File { hash, .. }) => Some(hash.to_wire()),
        Ok(store::Node::Dir) | Err(_) => None,
    };
    let mut request = Request::builder()
        .method(probe.method)
        .uri(target_uri(probe.target, &store));
    for (name, value) in probe.request_headers {
        request = request.header(
            *name,
            if *value == "{fixture-file-hash}" {
                fixture_file_hash
                    .as_deref()
                    .expect("probe fixture must include assets/app.js")
            } else {
                value
            },
        );
    }
    let response = app
        .clone()
        .oneshot(request.body(Body::from(probe.body)).unwrap())
        .await
        .unwrap();
    assert_eq!(
        response.status().as_u16(),
        probe.status,
        "{} response status",
        probe.endpoint
    );
    let declared = contract.outcome(probe.status, probe_variant(probe));
    assert!(
        declared.is_some(),
        "{} observed undeclared status {} variant {:?}",
        probe.endpoint,
        probe.status,
        probe_variant(probe)
    );
    for expected in probe.response_headers {
        assert!(
            contract
                .response_headers()
                .iter()
                .any(|name| name.eq_ignore_ascii_case(expected.name))
                || matches!(
                    expected.name,
                    "content-type"
                        | "cache-control"
                        | "www-authenticate"
                        | "content-range"
                        | "accept-ranges"
                ),
            "{} test expects undocumented response header {}",
            probe.endpoint,
            expected.name
        );
        let observed = response.headers().get(expected.name).unwrap_or_else(|| {
            panic!(
                "{} status {} omitted expected response header {}",
                probe.endpoint, probe.status, expected.name
            )
        });
        if let Some(value) = expected.value {
            assert_eq!(
                observed, value,
                "{} {} header",
                probe.endpoint, expected.name
            );
        }
    }
    let location = response
        .headers()
        .get(header::LOCATION)
        .map(|location| location.to_str().unwrap().to_string());
    assert_success_location_is_readable(probe, &app, location).await;
    assert_exact_outcome(probe, response).await;
}

async fn assert_success_location_is_readable(probe: Probe, app: &Router, location: Option<String>) {
    let endpoint = endpoint(probe.endpoint);
    if endpoint
        .exact_outcome(
            contract::OutcomeKind::Success,
            probe.status,
            probe_variant(probe),
        )
        .is_none()
    {
        return;
    }
    let Some(location) = location else {
        return;
    };
    let location = location.parse::<axum::http::Uri>().unwrap();
    let target = location
        .path_and_query()
        .map_or_else(|| location.path(), axum::http::uri::PathAndQuery::as_str);
    let fetched = app
        .clone()
        .oneshot(Request::builder().uri(target).body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert!(
        fetched.status().is_success() || fetched.status().is_redirection(),
        "{} returned unreadable Location {target}: {}",
        probe.endpoint,
        fetched.status()
    );
}

async fn assert_exact_outcome(probe: Probe, response: Response) {
    let expected = endpoint(probe.endpoint)
        .outcome(probe.status, probe_variant(probe))
        .unwrap_or_else(|| {
            panic!(
                "{} status {} variant {:?} lacks an exact outcome",
                probe.endpoint,
                probe.status,
                probe_variant(probe)
            )
        });
    for required in expected.required_headers {
        assert!(
            response.headers().contains_key(*required),
            "{} status {} omitted exact-outcome header {}",
            probe.endpoint,
            probe.status,
            required
        );
    }
    for forbidden in expected.forbidden_headers {
        assert!(
            !response.headers().contains_key(*forbidden),
            "{} status {} included forbidden exact-outcome header {}",
            probe.endpoint,
            probe.status,
            forbidden
        );
    }
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = body_bytes(response).await;
    match expected.body {
        contract::WireBody::Empty => assert!(body.is_empty()),
        contract::WireBody::Json => {
            assert!(content_type.starts_with("application/json"));
            serde_json::from_slice::<serde_json::Value>(&body).unwrap();
        }
        contract::WireBody::PlainText => {
            assert!(
                content_type.starts_with("text/"),
                "{} status {} returned {content_type}",
                probe.endpoint,
                probe.status
            );
            std::str::from_utf8(&body).unwrap();
        }
        contract::WireBody::Binary => assert!(!body.is_empty()),
    }
}

fn probe_variant(probe: Probe) -> contract::RequestVariant {
    if probe.endpoint != "allocated file" {
        return contract::RequestVariant::Default;
    }
    match probe
        .request_headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("allocation-action"))
        .map_or("create", |(_, value)| *value)
    {
        "create" => contract::RequestVariant::Create,
        "finalize" => contract::RequestVariant::Finalize,
        "propose" => contract::RequestVariant::Propose,
        "cancel" => contract::RequestVariant::Cancel,
        variant => panic!("unknown allocation probe variant {variant}"),
    }
}

async fn send(
    app: &Router,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: impl Into<Body>,
) -> Response {
    let mut request = Request::builder().method(method).uri(path);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    app.clone()
        .oneshot(request.body(body.into()).unwrap())
        .await
        .unwrap()
}

async fn fetch(app: &Router, path: &str) -> Response {
    send(app, "GET", path, &[], Body::empty()).await
}

/// `POST /hello/generated/` of an allocated file named from `asset-`.
async fn post_generated(
    app: &Router,
    extension: &str,
    extra_headers: &[(&str, &str)],
    idempotency_key: &str,
    body: &str,
) -> Response {
    let mut headers = vec![
        ("content-type", "application/octet-stream"),
        ("file-prefix", "asset-"),
        ("file-extension", extension),
    ];
    headers.extend_from_slice(extra_headers);
    headers.push(("idempotency-key", idempotency_key));
    send(app, "POST", "/hello/generated/", &headers, body.to_string()).await
}

async fn alias_latest(app: &Router, target: &str, idempotency_key: &str) -> Response {
    send(
        app,
        "ALIAS",
        "/hello/latest.js",
        &[
            ("alias-target", target),
            ("idempotency-key", idempotency_key),
        ],
        Body::empty(),
    )
    .await
}

async fn replace_at(
    app: &Router,
    path: &str,
    base_hash: &str,
    idempotency_key: &str,
    body: &str,
) -> Response {
    send(
        app,
        "REPLACE",
        path,
        &[
            ("if-content-match", base_hash),
            ("idempotency-key", idempotency_key),
        ],
        body.to_string(),
    )
    .await
}

async fn splice_at(
    app: &Router,
    path: &str,
    base_hash: &str,
    splice: &str,
    extra_headers: &[(&str, &str)],
    body: impl Into<Body>,
) -> Response {
    let mut headers = vec![("if-content-match", base_hash), ("splice", splice)];
    headers.extend_from_slice(extra_headers);
    send(app, "PATCH", path, &headers, body).await
}

fn declares_exact_outcome(endpoint_name: &str, kind: contract::OutcomeKind, status: u16) -> bool {
    endpoint(endpoint_name)
        .exact_outcome(kind, status, contract::RequestVariant::Default)
        .is_some()
}

/// `ALIAS` of a batch to a folder, optionally idempotent.
async fn alias_batch(
    app: &Router,
    path: &str,
    idempotency_key: Option<&str>,
    body: impl Into<Body>,
) -> Response {
    let mut headers = vec![("content-type", "application/json")];
    if let Some(key) = idempotency_key {
        headers.push(("idempotency-key", key));
    }
    send(app, "ALIAS", path, &headers, body).await
}

/// Borrow owned header values for [`send`].
fn borrowed<'a>(headers: &'a [(&'static str, String)]) -> Vec<(&'static str, &'a str)> {
    headers
        .iter()
        .map(|(name, value)| (*name, value.as_str()))
        .collect()
}

async fn json(response: Response) -> (HeaderMap, serde_json::Value) {
    let headers = response.headers().clone();
    let bytes = body_bytes(response).await;
    (
        headers,
        serde_json::from_slice(&bytes).unwrap_or_else(|error| {
            panic!(
                "expected JSON response ({error}): {}",
                String::from_utf8_lossy(&bytes)
            )
        }),
    )
}

async fn conditional_probe(endpoint_name: &str, path: &str) {
    let (_root, _store, app) = fixture(Fixture::Site).await;
    let first = fetch(&app, path).await;
    let etag = header_text(&first, "etag");
    let response = send(
        &app,
        "GET",
        path,
        &[("if-none-match", &etag)],
        Body::empty(),
    )
    .await;
    assert_eq!(
        response.status(),
        StatusCode::NOT_MODIFIED,
        "{endpoint_name}"
    );
    assert!(declares_exact_outcome(
        endpoint_name,
        contract::OutcomeKind::Success,
        304
    ));
    assert!(response.headers().contains_key("etag"));
    assert!(response.headers().contains_key("cache-control"));
}

#[tokio::test]
async fn every_contract_endpoint_executes_success_and_normative_error_probes() {
    assert_eq!(SUCCESS_PROBES.len(), contract::ENDPOINTS.len());
    for endpoint in contract::ENDPOINTS {
        assert_eq!(
            SUCCESS_PROBES
                .iter()
                .filter(|probe| probe.endpoint == endpoint.name)
                .count(),
            1,
            "{} must have exactly one primary success probe",
            endpoint.name
        );
    }
    for probe in SUCCESS_PROBES.iter().chain(ERROR_PROBES) {
        execute(*probe).await;
    }
}

#[tokio::test]
async fn allocated_post_rejects_reserved_control_directories_consistently() {
    let (root, store) = fresh_store();
    put(&store, "hello", "index.html", b"site");
    let token = store.operator_claim("hello").unwrap().encode();
    let authorization = format!("Bearer {token}");
    let reserved_body = format!("{}\n", contract::RESERVED_MUTATION_ERROR);
    let app = router(App::new(store.clone()));
    for path in [
        "/hello/FILES",
        "/hello/FILES/",
        "/hello/FILES/generated",
        "/hello/FILES/generated/",
        "/hello/UNDO",
        "/hello/UNDO/",
        "/hello/UNDO/x",
        "/hello/UNDO/x/",
        "/hello/EXPIRES",
        "/hello/EXPIRES/",
        "/hello/EXPIRES/x",
        "/hello/EXPIRES/x/",
        "/hello/safe/FILES",
        "/hello/safe/HASH",
        "/hello/safe/UNDO",
        "/hello/safe/EXPIRES",
    ] {
        for method in [
            "POST", "ALIAS", "REPLACE", "PATCH", "PUT", "DELETE", "EXPIRE",
        ] {
            let unauthorized = send(&app, method, path, &[], "must not spool").await;
            assert_eq!(
                unauthorized.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {path} must authenticate first"
            );
            assert_eq!(
                body_bytes(unauthorized).await,
                "error: management token required\n",
                "{method} {path} unauthorized body"
            );
            let rejected = send(
                &app,
                method,
                path,
                &[("authorization", &authorization)],
                "must not spool",
            )
            .await;
            assert_eq!(
                rejected.status(),
                StatusCode::BAD_REQUEST,
                "{method} {path}"
            );
            assert_eq!(
                body_bytes(rejected).await,
                reserved_body,
                "{method} {path} reserved-path body"
            );
        }
    }
    for path in [
        "FILES/generated",
        "UNDO/x",
        "EXPIRES/x",
        "safe/FILES",
        "safe/HASH",
        "safe/UNDO",
        "safe/EXPIRES",
    ] {
        assert!(matches!(
            store.lookup("hello", path),
            Err(StoreError::NotFound)
        ));
    }
    assert_eq!(
        tmp_entries(root.path()),
        0,
        "virtual namespace rejection must precede body spooling"
    );
}

#[tokio::test]
async fn cacheable_contract_endpoints_execute_conditional_requests() {
    for (name, path) in [
        ("docs", "/"),
        ("installer", "/install.sh"),
        ("client", "/symbol.sh"),
        ("site listing", "/FILES"),
        ("site index", "/hello/"),
        ("files subtree", "/hello/FILES/assets/"),
    ] {
        conditional_probe(name, path).await;
    }
}

#[tokio::test]
async fn mutation_contract_executes_noop_and_stale_write_paths() {
    let (_root, _store, app) = fixture(Fixture::Site).await;
    let no_op = send(
        &app,
        "PUT",
        "/hello",
        &[("content-type", "text/html")],
        "<h1>hello</h1>",
    )
    .await;
    assert_eq!(no_op.status(), StatusCode::OK);
    assert!(declares_exact_outcome(
        "site put",
        contract::OutcomeKind::Success,
        200
    ));
    assert!(!no_op.headers().contains_key("undo-token"));

    let inventory = send(
        &app,
        "GET",
        "/hello/FILES",
        &[("accept", "application/json")],
        Body::empty(),
    )
    .await;
    let stale_etag = header_text(&inventory, "etag");
    let update = send(
        &app,
        "PUT",
        "/hello/first.txt",
        &[("if-match", &stale_etag)],
        "first",
    )
    .await;
    assert_eq!(update.status(), StatusCode::OK);

    let stale = send(
        &app,
        "PUT",
        "/hello/raced.txt",
        &[("if-match", &stale_etag)],
        "must not commit",
    )
    .await;
    assert_eq!(stale.status(), StatusCode::PRECONDITION_FAILED);
    assert!(declares_exact_outcome(
        "file put",
        contract::OutcomeKind::Error,
        412
    ));
    assert!(stale.headers().contains_key("etag"));
    assert!(stale.headers().contains_key("content-revision"));
}

#[tokio::test]
#[expect(clippy::too_many_lines)]
async fn phase_five_mutations_cover_replay_conflict_limits_and_two_phase_outcomes() {
    let (_root, store, app) = fixture(Fixture::Site).await;

    let allocated = post_generated(&app, ".BIN", &[], "allocate-dropped", "allocated-body").await;
    assert_eq!(allocated.status(), StatusCode::CREATED);
    let (allocated_headers, allocated_json) = json(allocated).await;
    assert!(allocated_headers.contains_key("content-location"));
    assert_eq!(allocated_json["outcome"], "created");
    assert_eq!(allocated_json["naming"]["extension"], "bin");
    let allocated_path = allocated_json["path"].as_str().unwrap().to_string();
    let allocated_hash = allocated_json["hash"]
        .as_str()
        .unwrap()
        .trim_start_matches("blake3:")
        .to_string();

    let replay = post_generated(&app, "bin", &[], "allocate-dropped", "allocated-body").await;
    assert_eq!(replay.status(), StatusCode::CREATED);
    assert_replayed(&replay);
    let (_, replay_json) = json(replay).await;
    assert_eq!(replay_json["path"], allocated_path);
    assert_eq!(replay_json["replayed"], true);

    let no_op = post_generated(&app, "bin", &[], "allocate-noop", "allocated-body").await;
    assert_eq!(no_op.status(), StatusCode::OK);
    assert!(!no_op.headers().contains_key("undo-token"));
    let (_, no_op_json) = json(no_op).await;
    assert_eq!(no_op_json["outcome"], "existing");
    assert_eq!(no_op_json["changed"], false);

    let conflict = post_generated(&app, "bin", &[], "allocate-dropped", "different").await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);

    let expiring = post_generated(
        &app,
        "bin",
        &[("expiry-mode", "relative"), ("expiry-in", "1h")],
        "allocate-expiry",
        "allocated-body",
    )
    .await;
    assert_eq!(expiring.status(), StatusCode::OK);
    assert_eq!(
        own_expiry_mode(&store, "hello", &allocated_path),
        expiry::ExpiryMode::Relative
    );
    let allocated_route = format!("/hello/{allocated_path}");
    let relocate_headers = [
        ("if-content-match", allocated_hash.as_str()),
        ("idempotency-key", "allocated-replace-dropped"),
    ];
    let relocated = send(
        &app,
        "REPLACE",
        &allocated_route,
        &relocate_headers,
        "relocated allocated body",
    )
    .await;
    assert_eq!(relocated.status(), StatusCode::OK);
    let (_, relocated_json) = json(relocated).await;
    assert_eq!(relocated_json["outcome"], "relocated");
    assert_eq!(relocated_json["relocated"], true);
    let relocated_path = relocated_json["new_path"].as_str().unwrap().to_string();
    assert_ne!(relocated_path, allocated_path);
    assert!(matches!(
        store.lookup("hello", &allocated_path),
        Err(StoreError::NotFound)
    ));
    let relocated_replay = send(
        &app,
        "REPLACE",
        &allocated_route,
        &relocate_headers,
        "relocated allocated body",
    )
    .await;
    assert_eq!(relocated_replay.status(), StatusCode::OK);
    assert_replayed(&relocated_replay);
    assert_eq!(
        own_expiry_mode(&store, "hello", &relocated_path),
        expiry::ExpiryMode::Relative
    );

    let alias = alias_latest(&app, "assets/app.js", "alias-dropped").await;
    assert_eq!(alias.status(), StatusCode::CREATED);
    let alias_body = body_bytes(alias).await;
    let alias_replay = alias_latest(&app, "assets/app.js", "alias-dropped").await;
    assert_eq!(alias_replay.status(), StatusCode::CREATED);
    assert_replayed(&alias_replay);
    let replay_drift = alias_latest(&app, "index.html", "alias-retarget").await;
    assert_eq!(replay_drift.status(), StatusCode::OK);
    let replay_after_retarget = alias_latest(&app, "assets/app.js", "alias-dropped").await;
    let replay_after_retarget = body_bytes(replay_after_retarget).await;
    let mut original_alias: serde_json::Value = serde_json::from_slice(&alias_body).unwrap();
    let replay_after_retarget: serde_json::Value =
        serde_json::from_slice(&replay_after_retarget).unwrap();
    original_alias["replayed"] = serde_json::Value::Bool(true);
    assert_eq!(
        replay_after_retarget, original_alias,
        "only the replay marker may differ from the stored alias receipt"
    );
    assert_eq!(
        replay_after_retarget["target"], "assets/app.js",
        "alias replay must use its stored receipt rather than current state"
    );
    let changed_tree_guard = send(
        &app,
        "ALIAS",
        "/hello/latest.js",
        &[
            ("alias-target", "assets/app.js"),
            ("idempotency-key", "alias-dropped"),
            ("if-match", UNRELATED_TREE_HASH),
        ],
        Body::empty(),
    )
    .await;
    assert_eq!(changed_tree_guard.status(), StatusCode::CONFLICT);
    let alias_noop = alias_latest(&app, "index.html", "alias-noop").await;
    assert_eq!(alias_noop.status(), StatusCode::OK);
    assert!(!alias_noop.headers().contains_key("undo-token"));

    let batch = alias_batch(&app, "/hello/", Some("alias-batch"), r#"{"aliases":[{"path":"home","target":"index.html"},{"path":"app","target":"assets/app.js"}]}"#).await;
    assert_eq!(batch.status(), StatusCode::CREATED);
    let mixed_batch = alias_batch(&app, "/hello/", None, r#"{"aliases":[{"path":"home","target":"assets/app.js"},{"path":"new-link","target":"index.html"}]}"#).await;
    assert_eq!(
        mixed_batch.status(),
        StatusCode::OK,
        "a create+retarget batch is not wholly created"
    );
    let inventory = send(
        &app,
        "GET",
        "/hello/FILES",
        &[("accept", "application/json")],
        Body::empty(),
    )
    .await;
    let (_, inventory) = json(inventory).await;
    assert_eq!(inventory["aliases"].as_array().unwrap().len(), 4);
    let listing = send(
        &app,
        "GET",
        "/hello/FILES",
        &[("accept", "text/plain")],
        Body::empty(),
    )
    .await;
    let listing = body_bytes(listing).await;
    assert!(String::from_utf8_lossy(&listing).contains("latest.js -> index.html"));
    let stats = fetch(&app, "/STATS").await;
    let (_, stats) = json(stats).await;
    assert_eq!(stats["aliases"], 4);

    let original_hash = file_hash(&store, "hello", "assets/app.js");
    let replaced = replace_at(
        &app,
        "/hello/assets/app.js",
        &original_hash.to_wire(),
        "replace-dropped",
        "replacement",
    )
    .await;
    assert_eq!(replaced.status(), StatusCode::OK);
    let (_, replaced_json) = json(replaced).await;
    let replacement_hash = replaced_json["new_hash"]
        .as_str()
        .unwrap()
        .trim_start_matches("blake3:")
        .to_string();
    let replace_replay = replace_at(
        &app,
        "/hello/assets/app.js",
        &original_hash.to_wire(),
        "replace-dropped",
        "replacement",
    )
    .await;
    assert_eq!(replace_replay.status(), StatusCode::OK);
    assert_replayed(&replace_replay);
    let stale_replace = replace_at(
        &app,
        "/hello/assets/app.js",
        &original_hash.to_wire(),
        "replace-stale",
        "stale",
    )
    .await;
    assert_eq!(stale_replace.status(), StatusCode::PRECONDITION_FAILED);
    assert!(stale_replace.headers().contains_key("content-revision"));

    let splice = splice_at(
        &app,
        "/hello/assets/app.js",
        &replacement_hash,
        "offset=0; delete=1; insert=1",
        &[("idempotency-key", "splice-dropped")],
        "R",
    )
    .await;
    assert_eq!(splice.status(), StatusCode::OK);
    let (_, splice_json) = json(splice).await;
    let spliced_hash = splice_json["new_hash"]
        .as_str()
        .unwrap()
        .trim_start_matches("blake3:")
        .to_string();
    let splice_replay = splice_at(
        &app,
        "/hello/assets/app.js",
        &replacement_hash,
        "offset=0; delete=1; insert=1",
        &[("idempotency-key", "splice-dropped")],
        "R",
    )
    .await;
    assert_eq!(splice_replay.status(), StatusCode::OK);
    assert_replayed(&splice_replay);
    let range = splice_at(
        &app,
        "/hello/assets/app.js",
        &spliced_hash,
        "offset=999; delete=0; insert=0",
        &[],
        Body::empty(),
    )
    .await;
    assert_eq!(range.status(), StatusCode::RANGE_NOT_SATISFIABLE);

    let mut framed = Vec::from(b"SYMSPL1\0".as_slice());
    framed.extend_from_slice(&1_u32.to_be_bytes());
    framed.extend_from_slice(&0_u32.to_be_bytes());
    framed.extend_from_slice(&0_u64.to_be_bytes());
    framed.extend_from_slice(&0_u64.to_be_bytes());
    framed.extend_from_slice(&1_u64.to_be_bytes());
    framed.push(b'F');
    let framed_response = send(
        &app,
        "PATCH",
        "/hello/assets/app.js",
        &[
            ("if-content-match", &spliced_hash),
            ("content-type", "Application/Vnd.Symbol.Splice; VERSION=1"),
            ("idempotency-key", "splice-frame"),
        ],
        framed,
    )
    .await;
    assert_eq!(framed_response.status(), StatusCode::OK);

    let descriptors = vec!["offset=0; delete=0; insert=0"; 65].join(",");
    let over_limit = splice_at(
        &app,
        "/hello/assets/app.js",
        &spliced_hash,
        &descriptors,
        &[],
        Body::empty(),
    )
    .await;
    assert_eq!(over_limit.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let propose_headers = [
        ("allocation-action", "propose"),
        ("content-type", "text/plain"),
        ("expiry-mode", "relative"),
        ("expiry-in", "1h"),
        ("idempotency-key", "proposal-dropped"),
    ];
    let proposal = send(
        &app,
        "POST",
        "/hello/custom/",
        &propose_headers,
        "custom body",
    )
    .await;
    assert_eq!(proposal.status(), StatusCode::ACCEPTED);
    let proposal_etag = header_text(&proposal, header::ETAG);
    let proposal_revision = header_text(&proposal, "content-revision");
    let (_, proposal_json) = json(proposal).await;
    let proposal_token = proposal_json["allocation_token"]
        .as_str()
        .unwrap()
        .to_string();
    put(
        &store,
        "hello",
        "after-proposal.txt",
        b"intervening mutation",
    );
    let proposal_replay = send(
        &app,
        "POST",
        "/hello/custom/",
        &propose_headers,
        "custom body",
    )
    .await;
    assert_eq!(proposal_replay.status(), StatusCode::ACCEPTED);
    assert_replayed(&proposal_replay);
    assert_eq!(proposal_replay.headers()[header::ETAG], proposal_etag);
    assert_eq!(
        proposal_replay.headers()["content-revision"],
        proposal_revision
    );
    let changed_hint = send(
        &app,
        "POST",
        "/hello/custom/",
        &[
            ("allocation-action", "propose"),
            ("content-type", "text/plain"),
            ("file-extension", "bin"),
            ("expiry-mode", "relative"),
            ("expiry-in", "1h"),
            ("idempotency-key", "proposal-dropped"),
        ],
        "custom body",
    )
    .await;
    assert_eq!(changed_hint.status(), StatusCode::CONFLICT);
    let finalize_headers = [
        ("allocation-action", "finalize"),
        ("allocation-token", proposal_token.as_str()),
        ("file-name", "chosen"),
        ("idempotency-key", "finalize-dropped"),
    ];
    let finalized = send(
        &app,
        "POST",
        "/hello/custom/",
        &finalize_headers,
        Body::empty(),
    )
    .await;
    assert_eq!(finalized.status(), StatusCode::CREATED);
    let custom_content = fetch(&app, "/hello/custom/chosen").await;
    assert_eq!(custom_content.status(), StatusCode::OK);
    assert_eq!(
        custom_content.headers()[header::CONTENT_TYPE],
        "text/plain; charset=utf-8"
    );
    let custom_alias = send(
        &app,
        "ALIAS",
        "/hello/custom-link",
        &[("alias-target", "custom/chosen")],
        Body::empty(),
    )
    .await;
    assert_eq!(custom_alias.status(), StatusCode::CREATED);
    let custom_alias_content = fetch(&app, "/hello/custom-link").await;
    assert_eq!(
        custom_alias_content.headers()[header::CONTENT_TYPE],
        "text/plain; charset=utf-8"
    );
    let finalized_replay = send(
        &app,
        "POST",
        "/hello/custom/",
        &finalize_headers,
        Body::empty(),
    )
    .await;
    assert_eq!(finalized_replay.status(), StatusCode::CREATED);
    assert_replayed(&finalized_replay);
    assert_eq!(
        own_expiry_mode(&store, "hello", "custom/chosen"),
        expiry::ExpiryMode::Relative
    );

    let cancellable = send(
        &app,
        "POST",
        "/hello/custom/",
        &[
            ("allocation-action", "propose"),
            ("content-type", "application/octet-stream"),
            ("idempotency-key", "proposal-cancel"),
        ],
        "cancel me",
    )
    .await;
    let (_, cancellable) = json(cancellable).await;
    let cancel_token = cancellable["allocation_token"].as_str().unwrap();
    let wrong_folder = send(
        &app,
        "POST",
        "/hello/other/",
        &[
            ("allocation-action", "cancel"),
            ("allocation-token", cancel_token),
        ],
        Body::empty(),
    )
    .await;
    assert_eq!(wrong_folder.status(), StatusCode::NOT_FOUND);
    let cancel_tree = store.site_inventory("hello").unwrap().tree_hash;
    let cancel_headers = [
        ("allocation-action", "cancel"),
        ("allocation-token", cancel_token),
        ("idempotency-key", "cancel-dropped"),
        ("if-match", cancel_tree.as_str()),
    ];
    let cancelled = send(
        &app,
        "POST",
        "/hello/custom/",
        &cancel_headers,
        Body::empty(),
    )
    .await;
    assert_eq!(cancelled.status(), StatusCode::OK);
    let cancel_etag = header_text(&cancelled, header::ETAG);
    let cancel_revision = header_text(&cancelled, "content-revision");
    let (_, cancelled_body) = json(cancelled).await;
    put(&store, "hello", "after-cancel.txt", b"changed tree");
    let cancelled_replay = send(
        &app,
        "POST",
        "/hello/custom/",
        &cancel_headers,
        Body::empty(),
    )
    .await;
    assert_eq!(cancelled_replay.status(), StatusCode::OK);
    assert_replayed(&cancelled_replay);
    assert_eq!(cancelled_replay.headers()[header::ETAG], cancel_etag);
    assert_eq!(
        cancelled_replay.headers()["content-revision"],
        cancel_revision
    );
    let (_, cancelled_replay_body) = json(cancelled_replay).await;
    assert_eq!(
        cancelled_body["allocation_token"],
        cancelled_replay_body["allocation_token"]
    );
    let changed_cancel_guard = send(
        &app,
        "POST",
        "/hello/custom/",
        &[
            ("allocation-action", "cancel"),
            ("allocation-token", cancel_token),
            ("idempotency-key", "cancel-dropped"),
            ("if-match", UNRELATED_TREE_HASH),
        ],
        Body::empty(),
    )
    .await;
    assert_eq!(changed_cancel_guard.status(), StatusCode::CONFLICT);

    let media = send(
        &app,
        "POST",
        "/hello/media/",
        &[("content-type", "image/png"), ("file-extension", "txt")],
        "not really png",
    )
    .await;
    let (_, media) = json(media).await;
    let media_path = media["path"].as_str().unwrap();
    let media_response = fetch(&app, &format!("/hello/{media_path}")).await;
    assert_eq!(media_response.headers()[header::CONTENT_TYPE], "image/png");
}

async fn fetch_absolute_location(app: &Router, location: &str) -> Response {
    let location = location.parse::<axum::http::Uri>().unwrap();
    let target = location
        .path_and_query()
        .map_or_else(|| location.path(), axum::http::uri::PathAndQuery::as_str);
    fetch(app, target).await
}

/// A mutation response names `expected` both in `Location` and in its JSON
/// body, and that URL serves `content`.
async fn assert_location_fetchable(
    app: &Router,
    headers: &HeaderMap,
    body: &serde_json::Value,
    expected: &str,
    content: &str,
) {
    let location = headers[header::LOCATION].to_str().unwrap();
    assert_eq!(location, expected);
    assert_eq!(body["location"], expected);
    let fetched = fetch_absolute_location(app, location).await;
    assert_eq!(fetched.status(), StatusCode::OK);
    assert_eq!(body_bytes(fetched).await, content);
}

#[tokio::test]
#[expect(clippy::too_many_lines)]
async fn phase_five_mutation_urls_encode_each_stored_path_segment_and_are_fetchable() {
    const ENCODED_SEGMENT: &str = "100%25%20caf%C3%A9%20%3F%23";
    let (_root, store) = fresh_store();
    let raw_folder = "paths/100% café ?#";
    put(&store, "encoded", "target.txt", b"alias target");
    put(
        &store,
        "encoded",
        &format!("{raw_folder}/replace.txt"),
        b"replace before",
    );
    put(
        &store,
        "encoded",
        &format!("{raw_folder}/splice.txt"),
        b"splice before",
    );
    let app = router(App::new(store.clone()));

    let alias = send(
        &app,
        "ALIAS",
        &format!("/encoded/paths/{ENCODED_SEGMENT}/alias.txt"),
        &[("alias-target", "../../target.txt")],
        Body::empty(),
    )
    .await;
    assert_eq!(alias.status(), StatusCode::CREATED);
    let (alias_headers, alias_json) = json(alias).await;
    let expected_alias = format!("http://symbol/encoded/paths/{ENCODED_SEGMENT}/alias.txt");
    assert_location_fetchable(
        &app,
        &alias_headers,
        &alias_json,
        &expected_alias,
        "alias target",
    )
    .await;

    let allocated = send(
        &app,
        "POST",
        &format!("/encoded/paths/{ENCODED_SEGMENT}/"),
        &[("file-extension", "bin")],
        "allocated body",
    )
    .await;
    assert_eq!(allocated.status(), StatusCode::CREATED);
    let (allocated_headers, allocated_json) = json(allocated).await;
    let expected_allocated = format!(
        "http://symbol/encoded/paths/{ENCODED_SEGMENT}/{}",
        allocated_json["name"].as_str().unwrap()
    );
    assert_eq!(allocated_json["url"], expected_allocated);
    assert_location_fetchable(
        &app,
        &allocated_headers,
        &allocated_json,
        &expected_allocated,
        "allocated body",
    )
    .await;

    let replace_path = format!("{raw_folder}/replace.txt");
    let replace_hash = file_hash(&store, "encoded", &replace_path);
    let replaced = send(
        &app,
        "REPLACE",
        &format!("/encoded/paths/{ENCODED_SEGMENT}/replace.txt"),
        &[("if-content-match", &replace_hash.to_wire())],
        "replace after",
    )
    .await;
    assert_eq!(replaced.status(), StatusCode::OK);
    let (replaced_headers, replaced_json) = json(replaced).await;
    let expected_replaced = format!("http://symbol/encoded/paths/{ENCODED_SEGMENT}/replace.txt");
    assert_location_fetchable(
        &app,
        &replaced_headers,
        &replaced_json,
        &expected_replaced,
        "replace after",
    )
    .await;

    let splice_path = format!("{raw_folder}/splice.txt");
    let splice_hash = file_hash(&store, "encoded", &splice_path);
    let spliced = splice_at(
        &app,
        &format!("/encoded/paths/{ENCODED_SEGMENT}/splice.txt"),
        &splice_hash.to_wire(),
        "offset=0; delete=6; insert=6",
        &[],
        "after ",
    )
    .await;
    assert_eq!(spliced.status(), StatusCode::OK);
    let (spliced_headers, spliced_json) = json(spliced).await;
    let expected_spliced = format!("http://symbol/encoded/paths/{ENCODED_SEGMENT}/splice.txt");
    assert_location_fetchable(
        &app,
        &spliced_headers,
        &spliced_json,
        &expected_spliced,
        "after  before",
    )
    .await;
}

#[tokio::test]
async fn alias_batch_rejects_manifest_unsafe_paths_and_accepts_safe_punctuation() {
    let (_root, store) = fresh_store();
    put(&store, "alias-input", "target.txt", b"target");
    let app = router(App::new(store));
    for path in [
        "line\nbreak",
        "bell\u{7}path",
        ".DS_Store",
        "nested/Thumbs.db",
    ] {
        let body = serde_json::json!({
            "aliases": [{"path": path, "target": "target.txt"}]
        })
        .to_string();
        let rejected = alias_batch(&app, "/alias-input/", None, body).await;
        assert_eq!(rejected.status(), StatusCode::BAD_REQUEST, "{path:?}");
    }

    let safe_path = "quote\" ' []{}=+,;!@~$^&()#%?.txt";
    let body = serde_json::json!({
        "aliases": [{"path": safe_path, "target": "target.txt"}]
    })
    .to_string();
    let accepted = alias_batch(&app, "/alias-input/", None, body).await;
    assert_eq!(accepted.status(), StatusCode::CREATED);
    let (_, accepted) = json(accepted).await;
    assert_eq!(accepted["aliases"][0]["path"], safe_path);
}

#[tokio::test]
async fn alias_responses_inherit_target_and_intermediate_expiry_caps() {
    let (_root, store) = fresh_store();
    put(&store, "expiry-alias", "target.txt", b"target");
    put(&store, "expiry-alias", "directory/item.txt", b"item");
    put(&store, "expiry-alias", "links/anchor.txt", b"anchor");
    set_relative_expiry(&store, "expiry-alias", "target.txt", 80);
    set_relative_expiry(&store, "expiry-alias", "directory/item.txt", 30);
    let aliases = [
        ("links/target-capped", "../target.txt"),
        ("links/direct", "../target.txt"),
        ("links/chain", "direct"),
        ("view", "directory"),
        ("dangling", "missing"),
    ]
    .map(|(path, target)| store::AliasSpec { path, target });
    store
        .put_aliases(
            "expiry-alias",
            &aliases,
            store::FileMutationOptions::default(),
        )
        .unwrap();
    set_relative_expiry(&store, "expiry-alias", "links", 120);
    set_relative_expiry(&store, "expiry-alias", "links/direct", 40);
    set_relative_expiry(&store, "expiry-alias", "links/chain", 60);
    set_relative_expiry(&store, "expiry-alias", "dangling", 45);
    let app = router(App::new(store));

    let target = fetch(&app, "/expiry-alias/target.txt").await;
    let target_capped = fetch(&app, "/expiry-alias/links/target-capped").await;
    let direct = fetch(&app, "/expiry-alias/links/direct").await;
    let chain = fetch(&app, "/expiry-alias/links/chain").await;
    assert!(target.headers().contains_key(header::EXPIRES));
    assert_eq!(
        target_capped.headers()[header::EXPIRES],
        target.headers()[header::EXPIRES],
        "alias response must inherit its resolved target cap"
    );
    assert_ne!(
        direct.headers()[header::EXPIRES],
        target.headers()[header::EXPIRES],
        "direct alias policy must remain an independent earlier cap"
    );
    assert_eq!(
        chain.headers()[header::EXPIRES],
        direct.headers()[header::EXPIRES],
        "alias chains must inherit intermediate alias caps"
    );

    let item = fetch(&app, "/expiry-alias/directory/item.txt").await;
    let through_directory = fetch(&app, "/expiry-alias/view/item.txt").await;
    assert_eq!(
        through_directory.headers()[header::EXPIRES],
        item.headers()[header::EXPIRES],
        "directory alias descendants must inherit the resolved file cap"
    );

    let dangling = fetch(&app, "/expiry-alias/dangling").await;
    assert_eq!(dangling.status(), StatusCode::NOT_FOUND);
    let dangling_report = fetch(&app, "/expiry-alias/dangling/EXPIRES").await;
    assert_eq!(dangling_report.status(), StatusCode::OK);
    let (_, dangling_report) = json(dangling_report).await;
    assert!(dangling_report["effective_expires_at"].is_string());
}

#[tokio::test]
async fn phase_five_content_mutations_report_and_store_sanitized_bytes() {
    let (_root, store) = fresh_store();
    put(&store, "redacted", "replace.txt", b"before");
    put(&store, "redacted", "splice.txt", b"prefix:");
    let app = router(App::new(store.clone()));
    let management = format!("sym_mgmt_{}", "a".repeat(64));
    let claim = format!("sym_claim_{}", "b".repeat(64));
    let redacted_management = format!("sym_mgmt_{}", "*".repeat(64));
    let redacted_claim = format!("sym_claim_{}", "*".repeat(64));

    let allocated = send(
        &app,
        "POST",
        "/redacted/generated/",
        &[("file-extension", "txt")],
        format!("{management}\n{claim}"),
    )
    .await;
    assert_eq!(allocated.status(), StatusCode::CREATED);
    assert_eq!(allocated.headers()["sanitized-management-tokens"], "1");
    assert_eq!(allocated.headers()["sanitized-creator-claims"], "1");
    let (_, allocated_json) = json(allocated).await;
    assert_eq!(allocated_json["sanitized_management_tokens"], 1);
    assert_eq!(allocated_json["sanitized_creator_claims"], 1);
    let allocation_bytes = format!("{redacted_management}\n{redacted_claim}");
    let allocation_hash = blake3::hash(allocation_bytes.as_bytes())
        .to_hex()
        .to_string();
    assert_eq!(allocated_json["hash"], format!("blake3:{allocation_hash}"));
    assert!(
        allocated_json["path"]
            .as_str()
            .unwrap()
            .contains(&allocation_hash)
    );
    let fetched = fetch_absolute_location(&app, allocated_json["url"].as_str().unwrap()).await;
    assert_eq!(body_bytes(fetched).await, allocation_bytes);

    let replacement_base = file_hash(&store, "redacted", "replace.txt");
    let replaced = send(
        &app,
        "REPLACE",
        "/redacted/replace.txt",
        &[("if-content-match", &replacement_base.to_wire())],
        claim,
    )
    .await;
    assert_eq!(replaced.status(), StatusCode::OK);
    assert_eq!(replaced.headers()["sanitized-creator-claims"], "1");
    let (_, replaced_json) = json(replaced).await;
    assert_eq!(replaced_json["sanitized_creator_claims"], 1);
    assert_eq!(
        replaced_json["new_hash"],
        format!(
            "blake3:{}",
            blake3::hash(redacted_claim.as_bytes()).to_hex()
        )
    );

    let splice_base = file_hash(&store, "redacted", "splice.txt");
    let spliced = splice_at(
        &app,
        "/redacted/splice.txt",
        &splice_base.to_wire(),
        &format!("offset=7; delete=0; insert={}", management.len()),
        &[],
        management,
    )
    .await;
    assert_eq!(spliced.status(), StatusCode::OK);
    assert_eq!(spliced.headers()["sanitized-management-tokens"], "1");
    let (_, spliced_json) = json(spliced).await;
    assert_eq!(spliced_json["sanitized_management_tokens"], 1);
    let expected_splice = format!("prefix:{redacted_management}");
    assert_eq!(
        spliced_json["new_hash"],
        format!(
            "blake3:{}",
            blake3::hash(expected_splice.as_bytes()).to_hex()
        )
    );
    let fetched = fetch_absolute_location(&app, spliced_json["location"].as_str().unwrap()).await;
    assert_eq!(body_bytes(fetched).await, expected_splice);
}

#[tokio::test]
async fn splice_result_limit_and_stale_guard_precede_materialization() {
    let (root, store) = fresh_store();
    put(&store, "bounded", "data.bin", b"abcd");
    let hash = file_hash(&store, "bounded", "data.bin");
    let app = router(App::with_max_file_size(store.clone(), 4));
    let too_large = splice_at(
        &app,
        "/bounded/data.bin",
        &hash.to_wire(),
        "offset=4; delete=0; insert=1",
        &[],
        "x",
    )
    .await;
    assert_eq!(too_large.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(
        store.read_blob(hash).unwrap().as_ref(),
        b"abcd",
        "failed splice must not replace the source"
    );
    assert_eq!(tmp_entries(root.path()), 0);

    put(&store, "bounded", "data.bin", b"xy");
    let stale = splice_at(
        &app,
        "/bounded/data.bin",
        &hash.to_wire(),
        "offset=999; delete=0; insert=0",
        &[],
        Body::empty(),
    )
    .await;
    assert_eq!(stale.status(), StatusCode::PRECONDITION_FAILED);
    assert!(stale.headers().contains_key("content-revision"));
    // The stale-hash ETag carries the blake3: prefix, like every other content
    // ETag, and matches the hash named in the body.
    let current = ContentHash::from(blake3::hash(b"xy"));
    assert_eq!(
        stale.headers()[header::ETAG],
        format!("\"{}\"", current.to_wire()).as_str()
    );
    let body = body_bytes(stale).await;
    let body = String::from_utf8(body.to_vec()).unwrap();
    assert!(
        body.contains(&current.to_wire()),
        "stale body must name the current hash in wire form: {body}"
    );
}

#[tokio::test]
async fn managed_phase_five_mutations_reject_before_spooling() {
    let (root, _store, app) = fixture(Fixture::Managed).await;
    for (method, path, headers) in [
        ("POST", "/hello/generated/", Vec::new()),
        (
            "REPLACE",
            "/hello/index.html",
            vec![("if-content-match", "0".repeat(64))],
        ),
        (
            "PATCH",
            "/hello/index.html",
            vec![
                ("if-content-match", "0".repeat(64)),
                ("splice", "offset=0; delete=0; insert=1".to_string()),
            ],
        ),
    ] {
        let borrowed = borrowed(&headers);
        let response = send(&app, method, path, &borrowed, "must not spool").await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(tmp_entries(root.path()), 0);
    }
}

#[tokio::test]
async fn pending_allocations_remain_bound_to_the_proposing_bearer() {
    let (_root, store) = fresh_store();
    put(&store, "managed", "index.html", b"site");
    let original = store.operator_claim("managed").unwrap();
    let app = router(App::new(store.clone()));
    let original_authorization = format!("Bearer {}", original.encode());
    let proposed = send(
        &app,
        "POST",
        "/managed/custom/",
        &[
            ("allocation-action", "propose"),
            ("authorization", &original_authorization),
            ("idempotency-key", "bearer-bound-proposal"),
        ],
        "pending",
    )
    .await;
    assert_eq!(proposed.status(), StatusCode::ACCEPTED);
    let (_, proposed) = json(proposed).await;
    let token = proposed["allocation_token"].as_str().unwrap();

    let replacement = store.operator_rotate("managed", &original).unwrap();
    let replacement_authorization = format!("Bearer {}", replacement.encode());
    let finalized = send(
        &app,
        "POST",
        "/managed/custom/",
        &[
            ("allocation-action", "finalize"),
            ("allocation-token", token),
            ("file-name", "chosen"),
            ("authorization", &replacement_authorization),
            ("idempotency-key", "bearer-bound-finalize"),
        ],
        Body::empty(),
    )
    .await;
    assert_eq!(finalized.status(), StatusCode::UNAUTHORIZED);
    assert!(matches!(
        store.lookup("managed", "custom/chosen"),
        Err(StoreError::NotFound)
    ));
}

#[tokio::test]
#[expect(clippy::too_many_lines)]
async fn every_phase_five_endpoint_exercises_stale_noop_conflict_and_limits() {
    let (root, store) = fresh_store();
    put(&store, "edge", "data.bin", b"data");
    let stale_tree = store.site_inventory("edge").unwrap().tree_hash;
    let hash = file_hash(&store, "edge", "data.bin");
    put(&store, "edge", "later.bin", b"later");
    let app = router(App::new(store.clone()));

    for (method, path, headers, body) in [
        (
            "ALIAS",
            "/edge/link",
            vec![
                ("alias-target", "data.bin".to_string()),
                ("if-match", stale_tree.clone()),
            ],
            String::new(),
        ),
        (
            "ALIAS",
            "/edge/",
            vec![
                ("content-type", "application/json".to_string()),
                ("if-match", stale_tree.clone()),
            ],
            r#"{"aliases":[{"path":"link","target":"data.bin"}]}"#.to_string(),
        ),
        (
            "POST",
            "/edge/generated/",
            vec![("if-match", stale_tree.clone())],
            "stale allocation".to_string(),
        ),
        (
            "REPLACE",
            "/edge/data.bin",
            vec![
                ("if-content-match", hash.to_wire()),
                ("if-match", stale_tree.clone()),
            ],
            "replace".to_string(),
        ),
        (
            "PATCH",
            "/edge/data.bin",
            vec![
                ("if-content-match", hash.to_wire()),
                ("if-match", stale_tree.clone()),
                ("splice", "offset=0; delete=0; insert=1".to_string()),
            ],
            "x".to_string(),
        ),
    ] {
        let borrowed = borrowed(&headers);
        let response = send(&app, method, path, &borrowed, body).await;
        assert_eq!(
            response.status(),
            StatusCode::PRECONDITION_FAILED,
            "{method} stale tree guard"
        );
        assert!(response.headers().contains_key("content-revision"));
    }
    assert!(matches!(
        store.lookup("edge", "generated"),
        Err(StoreError::NotFound) | Ok(store::Node::Dir)
    ));
    assert_eq!(tmp_entries(root.path()), 0);

    let replace_noop = replace_at(
        &app,
        "/edge/data.bin",
        &hash.to_wire(),
        "edge-replace-noop",
        "data",
    )
    .await;
    assert_eq!(replace_noop.status(), StatusCode::OK);
    assert!(!replace_noop.headers().contains_key("undo-token"));
    let replace_noop_etag = replace_noop.headers()["etag"].clone();
    put(&store, "edge", "after-noop.bin", b"after");
    let replace_noop_replay = replace_at(
        &app,
        "/edge/data.bin",
        &hash.to_wire(),
        "edge-replace-noop",
        "data",
    )
    .await;
    assert_eq!(replace_noop_replay.headers()["etag"], replace_noop_etag);
    assert_replayed(&replace_noop_replay);
    let splice_noop = splice_at(
        &app,
        "/edge/data.bin",
        &hash.to_wire(),
        "offset=0; delete=0; insert=0",
        &[("idempotency-key", "edge-splice-noop")],
        Body::empty(),
    )
    .await;
    assert_eq!(splice_noop.status(), StatusCode::OK);
    assert!(!splice_noop.headers().contains_key("undo-token"));
    let splice_noop_etag = splice_noop.headers()["etag"].clone();
    put(&store, "edge", "after-splice-noop.bin", b"after");
    let splice_noop_replay = splice_at(
        &app,
        "/edge/data.bin",
        &hash.to_wire(),
        "offset=0; delete=0; insert=0",
        &[("idempotency-key", "edge-splice-noop")],
        Body::empty(),
    )
    .await;
    assert_eq!(splice_noop_replay.headers()["etag"], splice_noop_etag);
    assert_replayed(&splice_noop_replay);

    let batch_body =
        r#"{"aliases":[{"path":"one","target":"data.bin"},{"path":"two","target":"data.bin"}]}"#;
    let first_batch = alias_batch(&app, "/edge/", Some("edge-batch-replay"), batch_body).await;
    assert_eq!(first_batch.status(), StatusCode::CREATED);
    let replay_batch = alias_batch(&app, "/edge/", Some("edge-batch-replay"), batch_body).await;
    assert_eq!(replay_batch.status(), StatusCode::CREATED);
    assert_replayed(&replay_batch);
    let noop_batch = alias_batch(&app, "/edge/", Some("edge-batch-noop"), batch_body).await;
    assert_eq!(noop_batch.status(), StatusCode::OK);
    assert!(!noop_batch.headers().contains_key("undo-token"));

    let conflict = alias_batch(&app, "/edge/", None, r#"{"aliases":[{"path":"rollback","target":"data.bin"},{"path":"data.bin","target":"later.bin"}]}"#).await;
    assert_eq!(conflict.status(), StatusCode::CONFLICT);
    assert!(matches!(
        store.alias("edge", "rollback"),
        Err(StoreError::NotFound)
    ));

    let aliases = (0..=4096)
        .map(|index| contract::AliasDefinition {
            path: format!("limit/{index}"),
            target: "data.bin".to_string(),
        })
        .collect::<Vec<_>>();
    let oversized_batch = serde_json::to_string(&contract::AliasBatchRequest { aliases }).unwrap();
    let batch_limit = alias_batch(&app, "/edge/", None, oversized_batch).await;
    assert_eq!(batch_limit.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let oversized_target = "x".repeat(upload::MAX_ALIAS_TARGET_BYTES + 1);
    let target_limit = send(
        &app,
        "ALIAS",
        "/edge/too-long",
        &[("alias-target", &oversized_target)],
        Body::empty(),
    )
    .await;
    assert_eq!(target_limit.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let limited = router(App::with_max_file_size(store, 4));
    let allocation_limit = send(&limited, "POST", "/edge/generated/", &[], "12345").await;
    assert_eq!(allocation_limit.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let replace_limit = send(
        &limited,
        "REPLACE",
        "/edge/data.bin",
        &[("if-content-match", &hash.to_wire())],
        "12345",
    )
    .await;
    assert_eq!(replace_limit.status(), StatusCode::PAYLOAD_TOO_LARGE);
}
