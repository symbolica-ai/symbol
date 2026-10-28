//! Search: `GET /SEARCH` and its `OpenSearch` descriptor.
//!
//! By default, `by=name`, a query is words separated by spaces, and an entry
//! must match every word, ignoring case. A word matches the last segment of a
//! file or folder path: as a substring, or, when it holds `*` or `?`, as a
//! glob over the whole name. A word holding `/` matches the path instead,
//! written `/path` for a file and `/path/` for a folder: as a substring, or
//! as a glob that ends the path and starts at a folder boundary.
//!
//! With `by=content` the query is the language in [`query`], and the hits
//! are the live files whose text matches, from the index that [`index`]
//! keeps and [`extract`] fills; `store::search` holds the SQL.

pub mod extract;
pub mod index;
pub mod query;

use std::collections::HashMap;
use std::fmt::Write as _;

use axum::body::Body;
use axum::extract::{OriginalUri, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use maud::{DOCTYPE, Markup, PreEscaped, html};
use symbol_contract::{
    ContentSearchHit, ContentSearchResults, SearchBy, SearchHit, SearchHitKind, SearchResults,
    SnippetPart,
};

use crate::api_error::ApiError;
use crate::app::App;
use crate::browse::{
    self, BASE_STYLE, HumanSize, ListingLayout, Paging, PlainRow, STYLE, SizeLayout, TSV_TYPE,
};
use crate::mutation_http::encode_path_segment;
use crate::page;
use crate::store::{self, SearchCandidate, normalize_search_prefix};

pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 1000;
const MAX_QUERY_BYTES: usize = 256;
const MAX_WORDS: usize = 16;
const TSV_HEADER: &str = "kind\tsite\tpath\tfiles\tbytes\tmodified";
const CONTENT_TSV_HEADER: &str = "site\tpath\tbytes\tmodified\ttitle\tsnippet";
/// Files whose text is not yet searchable, on every content response.
const PENDING_HEADER: &str = "search-pending";

#[derive(Debug, Default, serde::Deserialize)]
pub struct SearchQuery {
    q: Option<String>,
    site: Option<String>,
    path: Option<String>,
    by: Option<String>,
    limit: Option<String>,
    page: Option<String>,
}

/// The `by` parameter: names unless it says otherwise. Parsed by hand, as
/// `limit` and `page` are, so a bad value gets the same `error:` line.
fn parse_by(value: Option<&str>) -> Result<SearchBy, ApiError> {
    match value.map(str::trim) {
        None | Some("") => Ok(SearchBy::Name),
        Some(value) => value
            .parse()
            .map_err(|()| "error: by must be name or content\n".into()),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Pattern {
    Contains(String),
    Glob(Vec<char>),
}

/// One lowercased query word.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Term {
    pattern: Pattern,
    /// Matches the path rather than the name.
    path: bool,
}

impl Term {
    fn parse(word: &str) -> Self {
        let word = word.to_lowercase();
        let path = word.contains('/');
        let pattern = if word.contains(['*', '?']) {
            Pattern::Glob(word.chars().collect())
        } else {
            Pattern::Contains(word)
        };
        Self { pattern, path }
    }

    /// A literal that every matching path contains, for the SQL prefilter.
    /// `LIKE` folds ASCII case only, so a fragment with anything else in it
    /// would wrongly exclude rows, and is not used.
    fn fragment(&self) -> Option<String> {
        let literal: String = match &self.pattern {
            Pattern::Contains(text) => text.clone(),
            Pattern::Glob(glob) => glob
                .split(|character| matches!(character, '*' | '?'))
                .max_by_key(|run| run.len())
                .unwrap_or_default()
                .iter()
                .collect(),
        };
        // Stored paths never start with `/`; only the matcher's target does.
        let literal = literal.trim_start_matches('/');
        (!literal.is_empty() && literal.is_ascii()).then(|| literal.to_string())
    }

    /// `None` if this term rejects the entry; otherwise how closely the name
    /// matched: 0 exactly (or by glob), 1 as a prefix, 2 anywhere. Path
    /// terms do not rank.
    fn rank(&self, name: &str, target: &str) -> Option<u8> {
        match (&self.pattern, self.path) {
            (Pattern::Contains(text), false) => {
                if name == text {
                    Some(0)
                } else if name.starts_with(text.as_str()) {
                    Some(1)
                } else {
                    name.contains(text.as_str()).then_some(2)
                }
            }
            (Pattern::Glob(glob), false) => {
                glob_matches(glob, &name.chars().collect::<Vec<_>>()).then_some(0)
            }
            (Pattern::Contains(text), true) => target.contains(text.as_str()).then_some(0),
            (Pattern::Glob(glob), true) => {
                let target: Vec<char> = target.chars().collect();
                (0..target.len())
                    .filter(|&start| start == 0 || target[start - 1] == '/')
                    .any(|start| glob_matches(glob, &target[start..]))
                    .then_some(0)
            }
        }
    }
}

/// `*` matches any run of characters, `?` any one; everything else itself.
fn glob_matches(glob: &[char], text: &[char]) -> bool {
    let (mut g, mut t) = (0, 0);
    let mut backtrack: Option<(usize, usize)> = None;
    while t < text.len() {
        match glob.get(g) {
            Some('*') => {
                backtrack = Some((g, t));
                g += 1;
            }
            Some(&character) if character == '?' || character == text[t] => {
                g += 1;
                t += 1;
            }
            _ => match backtrack {
                Some((star, from)) => {
                    g = star + 1;
                    t = from + 1;
                    backtrack = Some((star, from + 1));
                }
                None => return false,
            },
        }
    }
    glob[g..].iter().all(|&character| character == '*')
}

/// How every term ranks `path`, the worst one deciding. `path_words` says
/// whether any term needs the whole path.
fn rank(terms: &[Term], path_words: bool, path: &str, folder: bool) -> Option<u8> {
    let lower = path.to_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    let target = match (path_words, folder) {
        (false, _) => String::new(),
        (true, false) => format!("/{lower}"),
        (true, true) => format!("/{lower}/"),
    };
    terms
        .iter()
        .try_fold(0, |worst, term| Some(term.rank(name, &target)?.max(worst)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Hit {
    kind: SearchHitKind,
    site: String,
    path: String,
    files: u64,
    bytes: u64,
    modified: i64,
    /// The sort key ahead of site and path: rank, depth, name length, and
    /// whether this is a file.
    order: (u8, usize, usize, bool),
}

impl Hit {
    fn matching(
        terms: &[Term],
        path_words: bool,
        site: &str,
        path: &str,
        kind: SearchHitKind,
    ) -> Option<Self> {
        let folder = matches!(kind, SearchHitKind::Directory);
        let rank = rank(terms, path_words, path, folder)?;
        let name = path.rsplit('/').next().unwrap_or(path);
        Some(Self {
            kind,
            site: site.to_string(),
            path: path.to_string(),
            files: 0,
            bytes: 0,
            modified: 0,
            order: (
                rank,
                path.matches('/').count(),
                name.chars().count(),
                !folder,
            ),
        })
    }

    fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    const fn is_folder(&self) -> bool {
        matches!(self.kind, SearchHitKind::Directory)
    }

    fn url(&self) -> String {
        let mut url = format!("/{}", self.site);
        for segment in self.path.split('/') {
            url.push('/');
            encode_path_segment(&mut url, segment);
        }
        if self.is_folder() {
            url.push('/');
        }
        url
    }

    fn to_wire(&self) -> SearchHit {
        SearchHit {
            kind: self.kind,
            site: self.site.clone(),
            path: self.path.clone(),
            name: self.name().to_string(),
            url: self.url(),
            files: self.is_folder().then_some(self.files),
            bytes: self.bytes,
            modified: browse::known_time(self.modified).map(browse::rfc3339),
        }
    }
}

/// Every matching file and folder, best first: exact names, then prefixes,
/// then the rest; shallower before deeper; shorter names first; folders
/// before files; then by site and path.
///
/// Only folders below `prefix` can match. Each folder's count, size and
/// newest change come from its files among the candidates, which hold all
/// of them (see `store::search`).
fn find(terms: &[Term], prefix: &str, candidates: &[SearchCandidate]) -> Vec<Hit> {
    let path_words = terms.iter().any(|term| term.path);
    let mut hits = Vec::new();
    let mut folders: HashMap<(&str, &str), Option<Hit>> = HashMap::new();
    for candidate in candidates {
        let (site, path) = (candidate.site.as_str(), candidate.path.as_str());
        let mut end = prefix.len();
        while let Some(offset) = path[end..].find('/') {
            let folder = &path[..end + offset];
            end += offset + 1;
            let entry = folders.entry((site, folder)).or_insert_with(|| {
                Hit::matching(terms, path_words, site, folder, SearchHitKind::Directory)
            });
            if let Some(hit) = entry {
                hit.files += 1;
                hit.bytes += candidate.bytes;
                hit.modified = hit.modified.max(candidate.modified);
            }
        }
        if let Some(mut hit) = Hit::matching(terms, path_words, site, path, SearchHitKind::File) {
            hit.files = 1;
            hit.bytes = candidate.bytes;
            hit.modified = candidate.modified;
            hits.push(hit);
        }
    }
    hits.extend(folders.into_values().flatten());
    hits.sort_unstable_by(|left, right| {
        left.order
            .cmp(&right.order)
            .then_with(|| left.site.cmp(&right.site))
            .then_with(|| left.path.cmp(&right.path))
    });
    hits
}

/// A validated request.
#[derive(Debug)]
struct Search {
    /// `q` with surrounding whitespace removed.
    query: String,
    by: SearchBy,
    /// The name words; empty for a content search.
    terms: Vec<Term>,
    /// The FTS5 expression of a content search with a query.
    expression: Option<String>,
    site: Option<String>,
    /// `dir/`, or empty.
    prefix: String,
    limit: usize,
    page: usize,
}

impl Search {
    /// `allow_empty` lets a browser open the search page before typing.
    fn parse(query: SearchQuery, allow_empty: bool) -> Result<Self, ApiError> {
        let by = parse_by(query.by.as_deref())?;
        let text = query.q.as_deref().unwrap_or("").trim().to_string();
        if text.is_empty() && !allow_empty {
            return Err("error: q is required\n".into());
        }
        if text.len() > MAX_QUERY_BYTES {
            return Err("error: q must be at most 256 bytes\n".into());
        }
        if text.split_whitespace().count() > MAX_WORDS {
            return Err("error: q must have at most 16 words\n".into());
        }
        let (terms, expression) = match by {
            SearchBy::Name => (text.split_whitespace().map(Term::parse).collect(), None),
            SearchBy::Content if text.is_empty() => (Vec::new(), None),
            SearchBy::Content => (Vec::new(), Some(query::to_match(&text)?)),
        };
        let site = query.site.filter(|site| !site.is_empty());
        let raw_path = query.path.unwrap_or_default();
        if site.is_none() && !raw_path.trim_matches('/').is_empty() {
            return Err("error: path needs a site\n".into());
        }
        let prefix = normalize_search_prefix(&raw_path)?;
        let number = |value: Option<String>, default: usize, max: usize| {
            value.map_or(Some(default), |value| {
                value
                    .parse::<usize>()
                    .ok()
                    .filter(|n| (1..=max).contains(n))
            })
        };
        let limit = number(query.limit, DEFAULT_LIMIT, MAX_LIMIT)
            .ok_or("error: limit must be a whole number from 1 to 1000\n")?;
        let page = number(query.page, 1, usize::MAX)
            .ok_or("error: page must be a whole number of at least 1\n")?;
        Ok(Self {
            query: text,
            by,
            terms,
            expression,
            site,
            prefix,
            limit,
            page,
        })
    }

    fn fragments(&self) -> Vec<String> {
        self.terms.iter().filter_map(Term::fragment).collect()
    }

    fn pages(&self, total: usize) -> usize {
        total.div_ceil(self.limit).max(1)
    }

    const fn offset(&self) -> usize {
        (self.page - 1).saturating_mul(self.limit)
    }

    fn slice<'a>(&self, hits: &'a [Hit]) -> &'a [Hit] {
        let start = self.offset().min(hits.len());
        &hits[start..(start + self.limit).min(hits.len())]
    }

    /// A `/SEARCH` URL for this search with `site`, `path` and `by` replaced.
    fn href(&self, site: Option<&str>, prefix: &str, by: SearchBy) -> String {
        let mut href = String::from("/SEARCH?q=");
        encode_path_segment(&mut href, &self.query);
        if let Some(site) = site {
            href.push_str("&site=");
            encode_path_segment(&mut href, site);
            if !prefix.is_empty() {
                href.push_str("&path=");
                encode_path_segment(&mut href, prefix);
            }
        }
        if by == SearchBy::Content {
            href.push_str("&by=");
            href.push_str(by.as_str());
        }
        href
    }

    fn scope_label(&self) -> String {
        match (&self.site, self.prefix.as_str()) {
            (None, _) => "all sites".to_string(),
            (Some(site), prefix) => format!("{site}/{prefix}"),
        }
    }

    fn title(&self) -> String {
        if self.query.is_empty() {
            "search".to_string()
        } else {
            format!("{} · search", self.query)
        }
    }
}

/// One content hit as rendered: the store's row with its URL and the
/// snippet split into runs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ContentHit {
    site: String,
    path: String,
    bytes: u64,
    modified: i64,
    title: String,
    snippet: Vec<SnippetPart>,
}

impl ContentHit {
    fn from_store(hit: store::ContentHit) -> Self {
        Self {
            site: hit.site,
            path: hit.path,
            bytes: hit.bytes.cast_unsigned(),
            modified: hit.modified,
            title: hit.title,
            snippet: snippet_parts(hit.snippet.as_deref().unwrap_or_default()),
        }
    }

    fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or(&self.path)
    }

    /// Where the file is served, with an index page as its directory, the
    /// way a listing links it.
    fn url(&self) -> String {
        let mut url = format!("/{}", self.site);
        let index = matches!(self.name(), "index.html" | "index.htm");
        let segments: Vec<&str> = self.path.split('/').collect();
        let kept = if index {
            &segments[..segments.len() - 1]
        } else {
            &segments[..]
        };
        for segment in kept {
            url.push('/');
            encode_path_segment(&mut url, segment);
        }
        if index {
            url.push('/');
        }
        url
    }

    fn to_wire(&self) -> ContentSearchHit {
        ContentSearchHit {
            site: self.site.clone(),
            path: self.path.clone(),
            url: self.url(),
            title: self.title.clone(),
            bytes: self.bytes,
            modified: browse::known_time(self.modified).map(browse::rfc3339),
            snippet: self.snippet.clone(),
        }
    }

    /// The snippet as the store marks it, for TSV.
    fn marked_snippet(&self) -> String {
        let mut out = String::new();
        for part in &self.snippet {
            if part.hit {
                out.push(extract::HIT_START);
                out.push_str(&part.text);
                out.push(extract::HIT_END);
            } else {
                out.push_str(&part.text);
            }
        }
        out
    }
}

/// Splits a snippet into runs at the hit markers.
fn snippet_parts(snippet: &str) -> Vec<SnippetPart> {
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut hit = false;
    for character in snippet.chars() {
        let boundary = match character {
            extract::HIT_START => Some(true),
            extract::HIT_END => Some(false),
            _ => None,
        };
        match boundary {
            Some(next) => {
                if !text.is_empty() {
                    parts.push(SnippetPart {
                        text: std::mem::take(&mut text),
                        hit,
                    });
                }
                hit = next;
            }
            None => text.push(character),
        }
    }
    if !text.is_empty() {
        parts.push(SnippetPart { text, hit });
    }
    parts
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Json,
    Tsv,
    Plain,
    Html,
}

impl Format {
    fn of(headers: &HeaderMap) -> Self {
        if browse::wants_tsv(headers) {
            Self::Tsv
        } else if browse::wants_json(headers) {
            Self::Json
        } else {
            match page::negotiate(headers) {
                page::Flavor::Html => Self::Html,
                page::Flavor::Plain | page::Flavor::Man => Self::Plain,
            }
        }
    }
}

pub async fn search(
    State(app): State<App>,
    OriginalUri(uri): OriginalUri,
    Query(query): Query<SearchQuery>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let format = Format::of(&headers);
    let search = Search::parse(query, format == Format::Html)?;
    match search.by {
        SearchBy::Name => search_names(&app, search, format, &uri).await,
        SearchBy::Content => search_contents(&app, search, format, &uri).await,
    }
}

async fn search_names(
    app: &App,
    search: Search,
    format: Format,
    uri: &Uri,
) -> Result<Response, ApiError> {
    let site = search.site.clone();
    let prefix = search.prefix.clone();
    let fragments = search.fragments();
    let searching = !search.terms.is_empty();
    let (candidates, sites) = app
        .run_store(move |store| {
            let candidates = if searching {
                store.search_candidates(site.as_deref(), &prefix, &fragments)?
            } else {
                Vec::new()
            };
            let sites = if format == Format::Html {
                store.site_names()?
            } else {
                Vec::new()
            };
            Ok((candidates, sites))
        })
        .await?;
    let hits = find(&search.terms, &search.prefix, &candidates);
    let total = hits.len();
    let page = search.slice(&hits);
    let (body, content_type) = match format {
        Format::Json => {
            let results = SearchResults {
                query: search.query.clone(),
                site: search.site.clone(),
                path: (!search.prefix.is_empty()).then(|| search.prefix.clone()),
                total: u64::try_from(total).expect("count fits in u64"),
                page: u64::try_from(search.page).expect("page fits in u64"),
                limit: u64::try_from(search.limit).expect("limit fits in u64"),
                results: page.iter().map(Hit::to_wire).collect(),
            };
            (
                serde_json::to_string(&results).expect("search results serialize"),
                "application/json",
            )
        }
        Format::Tsv => (render_tsv(page), TSV_TYPE),
        Format::Plain => (
            render_plain(&search, page, total, uri),
            "text/plain; charset=utf-8",
        ),
        Format::Html => (
            render_html(&search, page, total, &sites, uri),
            "text/html; charset=utf-8",
        ),
    };
    Ok(respond(&search, body, content_type, uri, total, None))
}

async fn search_contents(
    app: &App,
    search: Search,
    format: Format,
    uri: &Uri,
) -> Result<Response, ApiError> {
    let site = search.site.clone();
    let prefix = search.prefix.clone();
    let expression = search.expression.clone();
    let (limit, offset) = (search.limit, search.offset());
    let ((total, hits), sites) = app
        .run_store(move |store| {
            let page = match &expression {
                Some(expression) => {
                    store.search_content(site.as_deref(), &prefix, expression, limit, offset)?
                }
                None => (0, Vec::new()),
            };
            let sites = if format == Format::Html {
                store.site_names()?
            } else {
                Vec::new()
            };
            Ok((page, sites))
        })
        .await?;
    let total = usize::try_from(total).expect("count fits in usize");
    let hits: Vec<ContentHit> = hits.into_iter().map(ContentHit::from_store).collect();
    let pending = app.search_indexer.pending();
    let (body, content_type) = match format {
        Format::Json => {
            let results = ContentSearchResults {
                query: search.query.clone(),
                by: SearchBy::Content,
                site: search.site.clone(),
                path: (!search.prefix.is_empty()).then(|| search.prefix.clone()),
                total: u64::try_from(total).expect("count fits in u64"),
                page: u64::try_from(search.page).expect("page fits in u64"),
                limit: u64::try_from(search.limit).expect("limit fits in u64"),
                pending,
                results: hits.iter().map(ContentHit::to_wire).collect(),
            };
            (
                serde_json::to_string(&results).expect("search results serialize"),
                "application/json",
            )
        }
        Format::Tsv => (render_content_tsv(&hits), TSV_TYPE),
        Format::Plain => (
            render_content_plain(&search, &hits, total, uri),
            "text/plain; charset=utf-8",
        ),
        Format::Html => (
            render_content_html(&search, &hits, total, pending, &sites, uri),
            "text/html; charset=utf-8",
        ),
    };
    Ok(respond(
        &search,
        body,
        content_type,
        uri,
        total,
        Some(pending),
    ))
}

/// The response every format shares: the negotiation and paging headers,
/// and `Search-Pending` for a content search.
fn respond(
    search: &Search,
    body: String,
    content_type: &'static str,
    uri: &Uri,
    total: usize,
    pending: Option<u64>,
) -> Response {
    let mut response = (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-cache"),
            (header::VARY, "Accept, User-Agent"),
        ],
        Body::from(body),
    )
        .into_response();
    if let Some(pending) = pending {
        response.headers_mut().insert(
            PENDING_HEADER,
            HeaderValue::from_str(&pending.to_string()).expect("a number is a header value"),
        );
    }
    browse::add_paging_headers(
        &mut response,
        uri,
        Paging {
            limit: Some(search.limit),
            page: search.page,
        },
        total,
    );
    response
}

/// Lets a browser add Symbol as a search engine.
pub async fn opensearch(State(app): State<App>) -> Response {
    let origin = xml_escape(app.public_url.trim_end_matches('/'));
    let body = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<OpenSearchDescription xmlns="http://a9.com/-/spec/opensearch/1.1/">
  <ShortName>symbol</ShortName>
  <Description>Find files and folders on symbol by name</Description>
  <InputEncoding>UTF-8</InputEncoding>
  <Url type="text/html" method="get" template="{origin}/SEARCH?q={{searchTerms}}"/>
</OpenSearchDescription>
"#
    );
    let mut response = Response::new(Body::from(body));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/opensearchdescription+xml"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
    response
}

fn xml_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Stored paths hold no tab, newline or other control character, so no field
/// needs quoting.
fn render_tsv(hits: &[Hit]) -> String {
    let mut out = format!("{TSV_HEADER}\n");
    for hit in hits {
        let wire = hit.to_wire();
        let kind = if hit.is_folder() { "directory" } else { "file" };
        let files = wire
            .files
            .map(|files| files.to_string())
            .unwrap_or_default();
        let modified = wire.modified.unwrap_or_default();
        writeln!(
            out,
            "{kind}\t{}\t{}\t{files}\t{}\t{modified}",
            hit.site, hit.path, hit.bytes
        )
        .unwrap();
    }
    out
}

fn display_name(hit: &Hit) -> String {
    let slash = if hit.is_folder() { "/" } else { "" };
    format!("{}/{}{slash}", hit.site, hit.path)
}

fn render_plain(search: &Search, hits: &[Hit], total: usize, uri: &Uri) -> String {
    let names: Vec<String> = hits.iter().map(display_name).collect();
    let sizes: Vec<HumanSize> = hits.iter().map(|hit| HumanSize::new(hit.bytes)).collect();
    let layout = ListingLayout {
        name: names.iter().map(String::len).max().unwrap_or(0).max(12),
        count: hits
            .iter()
            .filter(|hit| hit.is_folder())
            .map(|hit| browse::digits(hit.files))
            .max()
            .unwrap_or(1)
            .max(2),
        size: SizeLayout::from_sizes(&sizes),
    };
    let mut out = String::new();
    for (hit, name) in hits.iter().zip(&names) {
        PlainRow {
            name,
            files: hit.is_folder().then_some(hit.files),
            bytes: hit.bytes,
            modified: Some(hit.modified),
            suffix: "",
        }
        .push(&mut out, layout);
    }
    push_plain_footer(&mut out, search, total, uri);
    out
}

/// The count, page and next-page line that ends a plain listing.
fn push_plain_footer(out: &mut String, search: &Search, total: usize, uri: &Uri) {
    let pages = search.pages(total);
    match total {
        0 => out.push_str("no matches\n"),
        1 => out.push_str("1 match\n"),
        _ => {
            write!(out, "{total} matches").unwrap();
            if pages > 1 {
                write!(out, " · page {} of {pages}", search.page).unwrap();
            }
            if search.page < pages {
                write!(out, " · next: {}", browse::page_href(uri, search.page + 1)).unwrap();
            }
            out.push('\n');
        }
    }
}

/// Content hits for the shell client. The title and snippet are one line
/// each with no tab or control character, and the hits in the snippet are
/// wrapped in U+E000 and U+E001, which never occur in indexed text.
fn render_content_tsv(hits: &[ContentHit]) -> String {
    let mut out = format!("{CONTENT_TSV_HEADER}\n");
    for hit in hits {
        let modified = browse::known_time(hit.modified)
            .map(browse::rfc3339)
            .unwrap_or_default();
        writeln!(
            out,
            "{}\t{}\t{}\t{modified}\t{}\t{}",
            hit.site,
            hit.path,
            hit.bytes,
            hit.title,
            hit.marked_snippet()
        )
        .unwrap();
    }
    out
}

/// Content hits for curl: like grep, the file and its title, then the
/// snippet with the matched words between asterisks.
fn render_content_plain(search: &Search, hits: &[ContentHit], total: usize, uri: &Uri) -> String {
    let mut out = String::new();
    for hit in hits {
        write!(out, "{}/{}", hit.site, hit.path).unwrap();
        if !hit.title.is_empty() {
            write!(out, "  {}", hit.title).unwrap();
        }
        out.push('\n');
        if !hit.snippet.is_empty() {
            out.push_str("    ");
            for part in &hit.snippet {
                if part.hit {
                    write!(out, "*{}*", part.text).unwrap();
                } else {
                    out.push_str(&part.text);
                }
            }
            out.push('\n');
        }
    }
    push_plain_footer(&mut out, search, total, uri);
    out
}

/// A name split into runs, each marked if a name word matched it. Only ASCII
/// names are marked, where lowercasing keeps every byte offset.
fn highlight<'a>(name: &'a str, terms: &[Term]) -> Vec<(bool, &'a str)> {
    if !name.is_ascii() {
        return vec![(false, name)];
    }
    let lower = name.to_ascii_lowercase();
    let mut marked = vec![false; name.len()];
    for term in terms {
        let (Pattern::Contains(text), false) = (&term.pattern, term.path) else {
            continue;
        };
        let mut from = 0;
        while let Some(at) = lower[from..].find(text.as_str()) {
            let start = from + at;
            marked[start..start + text.len()].fill(true);
            from = start + text.len().max(1);
        }
    }
    let mut runs: Vec<(bool, &str)> = Vec::new();
    let mut start = 0;
    for end in 1..=name.len() {
        if end == name.len() || marked[end] != marked[start] {
            runs.push((marked[start], &name[start..end]));
            start = end;
        }
    }
    runs
}

/// The page frame both kinds of search share: the header, the form and the
/// results region, which the script swaps as the reader types.
fn search_page(search: &Search, sites: &[String], placeholder: &str, results: &Markup) -> String {
    html! {
        (DOCTYPE)
        meta charset="utf-8";
        meta name="viewport" content="width=device-width, initial-scale=1";
        title { (search.title()) }
        (descriptor_link())
        style {
            (PreEscaped(BASE_STYLE))
            (PreEscaped(STYLE))
        }
        main {
            header {
                span { "SEARCH" }
                span { (search.scope_label()) }
                span {
                    a href="/FILES/" { "files" }
                    " · "
                    a href="/" { "docs" }
                }
            }
            form.search role="search" method="get" action="/SEARCH" {
                input type="search" name="q" value=(search.query)
                    placeholder=(placeholder) aria-label=(placeholder)
                    autocomplete="off" spellcheck="false" autofocus[search.query.is_empty()];
                select name="site" aria-label="site" {
                    option value="" selected[search.site.is_none()] { "all sites" }
                    @for site in sites {
                        option value=(site) selected[search.site.as_deref() == Some(site.as_str())] {
                            (site)
                        }
                    }
                }
                @if !search.prefix.is_empty() {
                    input type="hidden" name="path" value=(search.prefix);
                    a.chip href=(search.href(search.site.as_deref(), "", search.by)) title="search the whole site" {
                        (search.prefix) " ×"
                    }
                }
                @if search.by == SearchBy::Content {
                    input type="hidden" name="by" value=(search.by.as_str());
                }
                button { "search" }
            }
            #results aria-live="polite" { (results) }
        }
        (PreEscaped(SEARCH_SCRIPT))
    }
    .into_string()
}

fn pager(search: &Search, total: usize, uri: &Uri) -> Markup {
    let pages = search.pages(total);
    html! {
        @if pages > 1 {
            nav.pager {
                @if search.page > 1 {
                    a href=(browse::page_href(uri, search.page - 1)) rel="prev" { "‹ prev" }
                }
                @if search.page < pages {
                    a href=(browse::page_href(uri, search.page + 1)) rel="next" { "next ›" }
                }
            }
        }
    }
}

fn summary(search: &Search, total: usize) -> Markup {
    let pages = search.pages(total);
    html! {
        .summary {
            span {
                (total) @if total == 1 { " match" } @else { " matches" }
                @if pages > 1 { " · page " (search.page) " of " (pages) }
            }
        }
    }
}

fn render_html(search: &Search, hits: &[Hit], total: usize, sites: &[String], uri: &Uri) -> String {
    let results = html! {
        @if search.query.is_empty() {
            p.hint {
                "Type part of a file or folder name. "
                code { "*.pdf" } " matches by pattern; a word with a "
                code { "/" } " matches whole paths. Press " kbd { "/" } " to search from any listing."
            }
        } @else if total == 0 {
            p.hint {
                "No file or folder names match “" (search.query) "”."
                " " a href=(search.href(search.site.as_deref(), &search.prefix, SearchBy::Content)) { "Search page text instead" } "."
                @if search.site.is_some() {
                    " " a href=(search.href(None, "", SearchBy::Name)) { "Search all sites" } "."
                }
            }
        } @else {
            (summary(search, total))
            .list {
                .row.head role="row" {
                    span { "name" }
                    span.meta.files { "files" }
                    span.meta { "size" }
                    span.meta { "modified" }
                }
                @for hit in hits {
                    (result_row(hit, &search.terms))
                }
            }
            (pager(search, total, uri))
        }
    };
    search_page(search, sites, "file or folder name", &results)
}

fn render_content_html(
    search: &Search,
    hits: &[ContentHit],
    total: usize,
    pending: u64,
    sites: &[String],
    uri: &Uri,
) -> String {
    let results = html! {
        @if pending > 0 {
            p.hint { "Indexing " (pending) " new files — results may be incomplete." }
        }
        @if search.query.is_empty() {
            p.hint {
                "Type words from a page. Every word must appear; "
                code { "\"a phrase\"" } " must appear together, "
                code { "-word" } " must not, " code { "OR" } " takes either, and "
                code { "ext*" } " matches a prefix."
            }
        } @else if total == 0 {
            p.hint {
                "No pages contain “" (search.query) "”."
                " " a href=(search.href(search.site.as_deref(), &search.prefix, SearchBy::Name)) { "Search names instead" } "."
                @if search.site.is_some() {
                    " " a href=(search.href(None, "", SearchBy::Content)) { "Search all sites" } "."
                }
            }
        } @else {
            (summary(search, total))
            .list {
                @for hit in hits {
                    (content_row(hit))
                }
            }
            (pager(search, total, uri))
        }
    };
    search_page(search, sites, "words in page text", &results)
}

fn content_row(hit: &ContentHit) -> Markup {
    let name = hit.name();
    let parent = &hit.path[..hit.path.len() - name.len()];
    html! {
        a.row href=(hit.url()) {
            span.name {
                @if hit.title.is_empty() { (name) } @else { (hit.title) }
                " " span.dim { (hit.site) "/" (parent) (name) }
            }
            span.meta.files {}
            span.meta { (browse::size_label(hit.bytes)) }
            (browse::date_cell(Some(hit.modified)))
        }
        @if !hit.snippet.is_empty() {
            p.snippet {
                @for part in &hit.snippet {
                    @if part.hit { mark { (part.text) } } @else { (part.text) }
                }
            }
        }
    }
}

fn result_row(hit: &Hit, terms: &[Term]) -> Markup {
    let name = hit.name();
    let parent = &hit.path[..hit.path.len() - name.len()];
    html! {
        a.row href=(hit.url()) {
            span.name {
                span.dim { (hit.site) "/" (parent) }
                @for (marked, text) in highlight(name, terms) {
                    @if marked { mark { (text) } } @else { (text) }
                }
                @if hit.is_folder() { "/" }
            }
            span.meta.files { @if hit.is_folder() { (hit.files) } }
            span.meta { (browse::size_label(hit.bytes)) }
            (browse::date_cell(Some(hit.modified)))
        }
    }
}

fn descriptor_link() -> Markup {
    html! {
        link rel="search" type="application/opensearchdescription+xml"
            href="/SEARCH/opensearch.xml" title="symbol";
    }
}

/// The search box a listing page shows, searching below the directory it
/// lists (`site` and `rel`), or every site from the site list.
pub fn search_box(site: Option<&str>, rel: &str) -> Markup {
    let placeholder = match site {
        None => "search all sites".to_string(),
        Some(site) if rel.is_empty() => format!("search {site}/"),
        Some(site) => format!("search {site}/{rel}/"),
    };
    html! {
        (descriptor_link())
        form.search.mini role="search" method="get" action="/SEARCH" {
            input type="search" name="q" placeholder=(placeholder)
                aria-label=(placeholder) autocomplete="off" spellcheck="false";
            @if let Some(site) = site {
                input type="hidden" name="site" value=(site);
                @if !rel.is_empty() {
                    input type="hidden" name="path" value=(format!("{rel}/"));
                }
            }
        }
        (PreEscaped(SLASH_KEY_SCRIPT))
    }
}

/// `/` focuses the page's search box, unless the reader is already typing.
const SLASH_KEY_SCRIPT: &str = r"<script>
document.addEventListener('keydown', (e) => {
  const box = document.querySelector('form.search input[name=q]');
  if (e.key !== '/' || !box || e.ctrlKey || e.metaKey || e.altKey) return;
  if (/^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement?.tagName)) return;
  e.preventDefault();
  box.focus();
  box.select();
});
</script>";

/// Results as you type: fetch the same page and swap in its results, so
/// there is one rendering. Also local times (as in listings) and `/`.
const SEARCH_SCRIPT: &str = r"<script>
(() => {
  const local = () => {
    for (const t of document.querySelectorAll('time[datetime]')) {
      const d = new Date(t.dateTime);
      if (isNaN(d)) continue;
      const p = (n) => String(n).padStart(2, '0');
      t.textContent = `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
      t.title = d.toString();
    }
  };
  local();
  const form = document.querySelector('form.search');
  const box = form.querySelector('input[name=q]');
  let timer, pending;
  const update = async () => {
    const url = `${form.action}?${new URLSearchParams(new FormData(form))}`;
    pending?.abort();
    pending = new AbortController();
    try {
      const response = await fetch(url, { headers: { Accept: 'text/html' }, signal: pending.signal });
      const page = new DOMParser().parseFromString(await response.text(), 'text/html');
      const next = page.getElementById('results');
      if (!next) return;
      document.getElementById('results').replaceWith(next);
      document.title = page.title;
      history.replaceState(null, '', url);
      local();
    } catch {}
  };
  box.addEventListener('input', () => {
    clearTimeout(timer);
    timer = setTimeout(update, 150);
  });
  form.querySelector('select').addEventListener('change', update);
  document.addEventListener('keydown', (e) => {
    if (e.key !== '/' || e.ctrlKey || e.metaKey || e.altKey) return;
    if (/^(INPUT|TEXTAREA|SELECT)$/.test(document.activeElement?.tagName)) return;
    e.preventDefault();
    box.focus();
    box.select();
  });
})();
</script>";

#[cfg(test)]
mod tests {
    use super::*;

    fn terms(query: &str) -> Vec<Term> {
        query.split_whitespace().map(Term::parse).collect()
    }

    fn file(site: &str, path: &str, bytes: u64) -> SearchCandidate {
        SearchCandidate {
            site: site.to_string(),
            path: path.to_string(),
            bytes,
            modified: 1_000,
        }
    }

    /// Like the store, only offers candidates below `prefix`.
    fn found(query: &str, prefix: &str, candidates: &[SearchCandidate]) -> Vec<String> {
        let candidates: Vec<SearchCandidate> = candidates
            .iter()
            .filter(|candidate| candidate.path.starts_with(prefix))
            .cloned()
            .collect();
        find(&terms(query), prefix, &candidates)
            .iter()
            .map(display_name)
            .collect()
    }

    #[test]
    fn globs_match_whole_runs() {
        let glob = |pattern: &str, text: &str| {
            glob_matches(
                &pattern.chars().collect::<Vec<_>>(),
                &text.chars().collect::<Vec<_>>(),
            )
        };
        assert!(glob("*.pdf", "notes.pdf"));
        assert!(!glob("*.pdf", "notes.pdf.txt"));
        assert!(glob("fig-?.png", "fig-1.png"));
        assert!(!glob("fig-?.png", "fig-10.png"));
        assert!(glob("a*b*c", "aXXbYYc"));
        assert!(!glob("a*b*c", "aXXbYY"));
        assert!(glob("*", ""));
        assert!(glob("**x", "x"));
    }

    #[test]
    fn words_match_names_and_slashes_match_paths() {
        let tree = [
            file("nlab", "topos/index.html", 10),
            file("nlab", "topos-theory/index.html", 20),
            file("nlab", "pagefind/fragment/topos_1.pf_fragment", 5),
            file("nlab", "pagefind/index/a.pf_index", 5),
            file("other", "Topos.PDF", 7),
        ];
        assert_eq!(
            found("topos", "", &tree),
            [
                "nlab/topos/",
                "other/Topos.PDF",
                "nlab/topos-theory/",
                "nlab/pagefind/fragment/topos_1.pf_fragment",
            ]
        );
        // The folder name matches; its files are not repeated.
        assert_eq!(found("pagefind", "", &tree), ["nlab/pagefind/"]);
        assert_eq!(found("*.pdf", "", &tree), ["other/Topos.PDF"]);
        assert_eq!(
            found("pagefind/ index", "", &tree),
            ["nlab/pagefind/index/", "nlab/pagefind/index/a.pf_index"]
        );
        assert_eq!(
            found("/topos/", "", &tree),
            ["nlab/topos/", "nlab/topos/index.html"]
        );
        assert_eq!(
            found("pagefind/*.pf_index", "", &tree),
            ["nlab/pagefind/index/a.pf_index"]
        );
        assert_eq!(found("topos theory", "", &tree), ["nlab/topos-theory/"]);
        assert!(found("nothing", "", &tree).is_empty());
    }

    #[test]
    fn folders_count_their_files_and_respect_the_scope() {
        let tree = [
            file("s", "docs/a/one.txt", 3),
            file("s", "docs/a/two.txt", 4),
            file("s", "docs/b.txt", 5),
        ];
        let hits = find(&terms("a"), "", &tree);
        let folder = hits.iter().find(|hit| hit.path == "docs/a").unwrap();
        assert_eq!((folder.files, folder.bytes), (2, 7));
        assert_eq!(folder.to_wire().url, "/s/docs/a/");
        // `docs` holds the scope; only what is below it can match.
        assert_eq!(found("docs", "docs/", &tree), Vec::<String>::new());
        assert_eq!(
            found("txt", "docs/a/", &tree),
            ["s/docs/a/one.txt", "s/docs/a/two.txt"]
        );
    }

    #[test]
    fn fragments_feed_the_prefilter_only_when_like_can_use_them() {
        let fragments = |query: &str| -> Vec<String> {
            terms(query).iter().filter_map(Term::fragment).collect()
        };
        assert_eq!(fragments("Kan-Ext"), ["kan-ext"]);
        assert_eq!(fragments("figure-*.png"), ["figure-"]);
        assert_eq!(fragments("/files/"), ["files/"]);
        assert!(fragments("über *").is_empty());
    }

    #[test]
    fn urls_encode_each_segment() {
        let hit = &find(&terms("b"), "", &[file("s", "a b/c#d.txt", 1)])[0];
        assert_eq!(hit.to_wire().url, "/s/a%20b/");
        let hit = &find(&terms("c#"), "", &[file("s", "a b/c#d.txt", 1)])[0];
        assert_eq!(hit.to_wire().url, "/s/a%20b/c%23d.txt");
    }

    #[test]
    fn highlight_marks_every_occurrence_of_name_words() {
        assert_eq!(
            highlight("Kan-kan.txt", &terms("kan")),
            [(true, "Kan"), (false, "-"), (true, "kan"), (false, ".txt")]
        );
        assert_eq!(highlight("über", &terms("ber")), [(false, "über")]);
    }

    #[test]
    fn requests_are_validated() {
        let parse = |q: &str, extra: &[(&str, &str)]| {
            let mut query = SearchQuery {
                q: Some(q.to_string()),
                ..SearchQuery::default()
            };
            for (name, value) in extra {
                let value = Some((*value).to_string());
                match *name {
                    "site" => query.site = value,
                    "path" => query.path = value,
                    "limit" => query.limit = value,
                    "page" => query.page = value,
                    _ => unreachable!(),
                }
            }
            Search::parse(query, false).map(|search| (search.prefix, search.limit, search.page))
        };
        assert_eq!(
            parse("x", &[]).ok(),
            Some((String::new(), DEFAULT_LIMIT, 1))
        );
        assert_eq!(
            parse("x", &[("site", "s"), ("path", "/a/b/")]).ok(),
            Some(("a/b/".to_string(), DEFAULT_LIMIT, 1))
        );
        for (q, extra) in [
            ("", &[][..]),
            ("  ", &[]),
            (&"x".repeat(257), &[]),
            ("a b c d e f g h i j k l m n o p q", &[]),
            ("x", &[("path", "a")]),
            ("x", &[("site", "s"), ("path", "../a")]),
            ("x", &[("limit", "0")]),
            ("x", &[("limit", "1001")]),
            ("x", &[("page", "0")]),
            ("x", &[("page", "one")]),
        ] {
            assert!(parse(q, extra).is_err(), "{q:?} {extra:?}");
        }
    }
}
