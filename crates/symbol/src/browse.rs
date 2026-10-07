use std::collections::HashSet;
use std::fmt::Write as _;

use axum::body::Bytes;
use axum::http::{HeaderMap, HeaderValue, Uri, header};
use axum::response::Response;
use maud::{DOCTYPE, Markup, PreEscaped, html};
use symbol_contract::{Listing, ListingEntry, ListingKind};

use crate::http_cache::{self, Representation};
use crate::page;
use crate::pathutil::pretty_html_name;
use crate::store::{AliasEntry, AliasResolvedKind, DirEnt, DirList, EntryKind, SiteList};

pub fn sites(headers: &HeaderMap, uri: &Uri, query: &ListingQuery, mut list: SiteList) -> Response {
    let sort = match Sort::parse(query) {
        Ok(sort) => sort,
        Err(message) => return bad_query(message),
    };
    sort_sites(&mut list, sort);
    let list = &list;
    if wants_tsv(headers) {
        return match TsvRequest::parse(uri, query) {
            Ok(request) => tsv_response(
                headers,
                request.uri,
                sites_tsv(list, request.paging, request.columns),
                request.paging,
            ),
            Err(message) => bad_query(message),
        };
    }
    if wants_json(headers) {
        let mut entries = vec![ListingEntry {
            kind: ListingKind::Builtin,
            name: "API".to_string(),
            files: None,
            bytes: 0,
            target: None,
            target_kind: None,
            dangling: None,
        }];
        entries.extend(list.entries.iter().map(|entry| ListingEntry {
            kind: ListingKind::Site,
            name: entry.name.clone(),
            files: Some(entry.files),
            bytes: entry.bytes,
            target: None,
            target_kind: None,
            dangling: None,
        }));
        return json_response(
            headers,
            &Listing {
                path: "/".to_string(),
                files: list.files,
                aliases: list.alias_count,
                bytes: list.bytes,
                entries,
            },
        );
    }
    let flavor = page::negotiate(headers);
    match flavor {
        page::Flavor::Plain | page::Flavor::Man => {
            let body = render_sites_plain(list);
            cached_response(headers, body, "text/plain; charset=utf-8")
        }
        page::Flavor::Html => {
            let body = render_sites_html(list, sort);
            cached_response(headers, body, "text/html; charset=utf-8")
        }
    }
}

pub fn listing(
    headers: &HeaderMap,
    site: &str,
    rel: &str,
    mut list: DirList,
    files_view: bool,
    view: ListingView<'_>,
) -> Response {
    let sort = view.sort;
    sort_listing(rel, &mut list, sort);
    let list = &list;
    if let Some(request) = view.tsv {
        return tsv_response(
            headers,
            request.uri,
            listing_tsv(rel, list, request.paging, request.columns),
            request.paging,
        );
    }
    if wants_json(headers) {
        let mut entries = list
            .entries
            .iter()
            .map(|entry| ListingEntry {
                kind: match entry.kind {
                    EntryKind::Directory => ListingKind::Directory,
                    EntryKind::File => ListingKind::File,
                },
                name: entry.name.clone(),
                files: (entry.kind == EntryKind::Directory).then_some(entry.files),
                bytes: entry.bytes,
                target: None,
                target_kind: None,
                dangling: None,
            })
            .collect::<Vec<_>>();
        entries.extend(list.aliases.iter().filter_map(|alias| {
            direct_alias_name(rel, &alias.path).map(|name| ListingEntry {
                kind: ListingKind::Alias,
                name: name.to_string(),
                files: alias.resolved_files,
                bytes: alias.resolved_size.unwrap_or(0),
                target: Some(alias.canonical_target.clone()),
                target_kind: alias.resolved_kind.map(|kind| match kind {
                    AliasResolvedKind::File => symbol_contract::AliasTargetKind::File,
                    AliasResolvedKind::Directory => symbol_contract::AliasTargetKind::Directory,
                }),
                dangling: Some(alias.resolved_kind.is_none()),
            })
        }));
        return json_response(
            headers,
            &Listing {
                path: display_path(site, rel),
                files: list.files,
                aliases: list.alias_count,
                bytes: list.bytes,
                entries,
            },
        );
    }
    let flavor = page::negotiate(headers);
    match flavor {
        page::Flavor::Plain | page::Flavor::Man => {
            let body = render_plain(site, rel, list);
            cached_response(headers, body, "text/plain; charset=utf-8")
        }
        page::Flavor::Html => {
            let body = render_html(site, rel, list, files_view, sort);
            cached_response(headers, body, "text/html; charset=utf-8")
        }
    }
}

fn render_sites_plain(list: &SiteList) -> String {
    let mut sizes: Vec<HumanSize> = list
        .entries
        .iter()
        .map(|entry| HumanSize::new(entry.bytes))
        .collect();
    sizes.push(HumanSize::new(0));
    sizes.push(HumanSize::new(list.bytes));
    let name_width = list
        .entries
        .iter()
        .map(|entry| entry.name.len() + 1)
        .chain(std::iter::once("API/".len()))
        .max()
        .unwrap_or(0)
        .max(12);
    let count_width = list
        .entries
        .iter()
        .map(|entry| digits(entry.files))
        .chain(std::iter::once(digits(list.files)))
        .max()
        .unwrap()
        .max(2);
    let layout = ListingLayout {
        name: name_width,
        count: count_width,
        size: SizeLayout::from_sizes(&sizes),
    };
    let mut out = String::new();
    PlainRow {
        name: "API/",
        suffix: " built-in",
        ..PlainRow::default()
    }
    .push(&mut out, layout);
    for entry in &list.entries {
        PlainRow {
            name: &format!("{}/", entry.name),
            files: Some(entry.files),
            bytes: entry.bytes,
            modified: Some(entry.modified),
            ..PlainRow::default()
        }
        .push(&mut out, layout);
    }
    PlainRow {
        files: Some(list.files),
        bytes: list.bytes,
        suffix: " total",
        ..PlainRow::default()
    }
    .push(&mut out, layout);
    out
}

fn render_sites_html(list: &SiteList, sort: Sort) -> String {
    html! {
        (DOCTYPE)
        meta charset="utf-8";
        meta name="viewport" content="width=device-width, initial-scale=1";
        title { "sites" }
        style {
            (PreEscaped(BASE_STYLE))
            (PreEscaped(STYLE))
        }
        main {
            header {
                span { "FILES" }
                span { "sites" }
                span { a href="/" { "docs" } }
            }
            .summary {
                (list.files) " files · " (size_label(list.bytes)) " logical"
            }
            .list {
                (sort_header(sort))
                a.row href="/API/" {
                    span.name { "API/" }
                    span.meta.files {}
                    span.meta { "built-in" }
                    span.meta {}
                }
                @for entry in &list.entries {
                    a.row href=(format!("/{}/FILES/", entry.name)) {
                        span.name { (&entry.name) "/" }
                        span.meta.files { (entry.files) }
                        span.meta { (size_label(entry.bytes)) }
                        (date_cell(Some(entry.modified)))
                    }
                }
            }
        }
        (PreEscaped(LOCAL_TIME_SCRIPT))
    }
    .into_string()
}

/// The column headings, each a link that sorts by it. The active column shows
/// its direction and, clicked again, reverses it.
fn sort_header(sort: Sort) -> Markup {
    let column = |key: SortKey, label: &str, class: &str| {
        let active = sort.key == key;
        let aria = match (active, sort.descending) {
            (false, _) => "none",
            (true, false) => "ascending",
            (true, true) => "descending",
        };
        html! {
            a.sort.(class).active[active] href=(sort.header_query(key)) aria-sort=(aria)
                title=(format!("sort by {label}")) {
                (label)
                @if active {
                    span.arrow { @if sort.descending { " ▼" } @else { " ▲" } }
                }
            }
        }
    };
    html! {
        .row.head role="row" {
            (column(SortKey::Name, "name", "name-col"))
            (column(SortKey::Files, "files", "files"))
            (column(SortKey::Size, "size", "size-col"))
            (column(SortKey::Modified, "modified", "date-col"))
        }
    }
}

fn date_cell(modified: Option<i64>) -> Markup {
    html! {
        @if let Some(modified) = modified.and_then(known_time) {
            span.meta {
                time datetime=(rfc3339(modified)) title=(rfc3339(modified)) {
                    (date_label(modified))
                }
            }
        } @else {
            span.meta {}
        }
    }
}

fn render_plain(site: &str, rel: &str, list: &DirList) -> String {
    let display = display_path(site, rel);
    let mut sizes: Vec<HumanSize> = list
        .entries
        .iter()
        .map(|entry| HumanSize::new(entry.bytes))
        .collect();
    sizes.push(HumanSize::new(list.bytes));
    let name_width = list
        .entries
        .iter()
        .map(|entry| entry.name.len() + usize::from(entry.kind == EntryKind::Directory))
        .chain(std::iter::once(display.len()))
        .max()
        .unwrap()
        .max(12);
    let count_width = list
        .entries
        .iter()
        .filter(|entry| entry.kind == EntryKind::Directory)
        .map(|entry| digits(entry.files))
        .chain(std::iter::once(digits(list.files)))
        .max()
        .unwrap()
        .max(2);
    let layout = ListingLayout {
        name: name_width,
        count: count_width,
        size: SizeLayout::from_sizes(&sizes),
    };
    let mut out = String::new();
    PlainRow {
        name: &display,
        files: Some(list.files),
        bytes: list.bytes,
        ..PlainRow::default()
    }
    .push(&mut out, layout);
    if !rel.is_empty() {
        out.push_str("../\n");
    }
    for entry in &list.entries {
        let name = match entry.kind {
            EntryKind::Directory => format!("{}/", entry.name),
            EntryKind::File => entry.name.clone(),
        };
        PlainRow {
            name: &name,
            files: (entry.kind == EntryKind::Directory).then_some(entry.files),
            bytes: entry.bytes,
            modified: Some(entry.modified),
            ..PlainRow::default()
        }
        .push(&mut out, layout);
    }
    for alias in &list.aliases {
        if let Some(name) = direct_alias_name(rel, &alias.path) {
            write!(out, "{name} -> {}", alias.canonical_target).unwrap();
            if let Some(modified) = known_time(alias.modified) {
                write!(out, "   {}", date_label(modified)).unwrap();
            }
            out.push('\n');
        }
    }
    out
}

fn render_html(site: &str, rel: &str, list: &DirList, files_view: bool, sort: Sort) -> String {
    let display = display_path(site, rel);
    let parent = parent_href(site, rel, files_view);
    let files_href = format!("/{site}/FILES/");
    let site_href = format!("/{site}/");
    let occupied = listing_occupied_names(rel, list);
    let see_site = files_view
        && list.entries.iter().any(|entry| {
            entry.kind == EntryKind::File
                && matches!(entry.name.as_str(), "index.html" | "index.htm")
        });
    html! {
        (DOCTYPE)
        meta charset="utf-8";
        meta name="viewport" content="width=device-width, initial-scale=1";
        title { (&display) }
        style {
            (PreEscaped(BASE_STYLE))
            (PreEscaped(STYLE))
        }
        main {
            header {
                span { "FILES" }
                span { (&display) }
                span {
                    a href=(site_href) { "site" }
                    " · "
                    a href=(files_href) { "files" }
                }
            }
            .summary {
                span { (list.files) " files · " (size_label(list.bytes)) " logical" }
                @if see_site {
                    a.see-site href=(dir_href(site, rel, false)) { "see site" }
                }
            }
            .list {
                (sort_header(sort))
                @if let Some(href) = parent {
                    a.row href=(href) {
                        span.name { ".." }
                        span.meta.files {}
                        span.meta {}
                        span.meta {}
                    }
                }
                @for entry in &list.entries {
                    a.row href=(entry_href(site, rel, &entry.name, entry.kind, files_view, &occupied)) {
                        span.name {
                            (&entry.name)
                            @if entry.kind == EntryKind::Directory { "/" }
                        }
                        span.meta.files {
                            @if entry.kind == EntryKind::Directory { (entry.files) }
                        }
                        span.meta { (size_label(entry.bytes)) }
                        (date_cell(Some(entry.modified)))
                    }
                }
                @for alias in &list.aliases {
                    @if let Some(name) = direct_alias_name(rel, &alias.path) {
                        a.row href=(format!("/{site}/{}", alias.path)) {
                            span.name { (name) " -> " (&alias.canonical_target) }
                            span.meta.files {
                                @if let Some(files) = alias.resolved_files { (files) }
                            }
                            span.meta {
                                @if alias.resolved_kind.is_none() {
                                    "dangling"
                                } @else if let Some(size) = alias.resolved_size {
                                    (size_label(size))
                                } @else {
                                    "alias"
                                }
                            }
                            (date_cell(Some(alias.modified)))
                        }
                    }
                }
            }
        }
        (PreEscaped(LOCAL_TIME_SCRIPT))
    }
    .into_string()
}

/// Rewrites each `<time>` from the UTC the server rendered into the reader's
/// own time zone. Without script the UTC text stays, and is still correct.
const LOCAL_TIME_SCRIPT: &str = r"<script>
for (const t of document.querySelectorAll('time[datetime]')) {
  const d = new Date(t.dateTime);
  if (isNaN(d)) continue;
  const p = (n) => String(n).padStart(2, '0');
  t.textContent = `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
  t.title = d.toString();
}
</script>";

fn direct_alias_name<'a>(rel: &str, path: &'a str) -> Option<&'a str> {
    let suffix = if rel.is_empty() {
        path
    } else {
        path.strip_prefix(rel)?.strip_prefix('/')?
    };
    (!suffix.is_empty() && !suffix.contains('/')).then_some(suffix)
}

fn display_path(site: &str, rel: &str) -> String {
    if rel.is_empty() {
        format!("{site}/")
    } else {
        format!("{site}/{rel}/")
    }
}

/// The media type of the line-oriented listings: one entry per line, fields
/// separated by tabs, a header line naming them. Stored names contain no
/// control characters, so no field needs quoting, and any `awk` reads it.
pub const TSV_TYPE: &str = "text/tab-separated-values; charset=utf-8";

const LISTING_HEADER: &str = "kind\tfiles\tbytes\tname\ttarget";
/// Optional TSV listing columns, appended after the fixed ones in this order.
const OPTIONAL_COLUMNS: [&str; 1] = ["modified"];
const INVENTORY_HEADER: &str = "kind\tsize\tvalue\tpath";
/// The most entries one page may ask for.
pub const MAX_PAGE_LIMIT: usize = 100_000;

pub fn wants_tsv(headers: &HeaderMap) -> bool {
    accepts(headers, "text/tab-separated-values")
}

/// The query parameters of a listing.
///
/// `sort` and `order` apply to every representation of one directory or of
/// the site list. `recursive`, `limit`, `page`, and `columns` are read only
/// for TSV.
#[derive(Debug, Default, serde::Deserialize)]
pub struct ListingQuery {
    recursive: Option<String>,
    limit: Option<String>,
    page: Option<String>,
    sort: Option<String>,
    order: Option<String>,
    columns: Option<String>,
}

/// What a listing is ordered by. Folders always come before files, and
/// aliases after both; the key orders entries within each group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Files,
    Size,
    Modified,
}

impl SortKey {
    const ALL: [Self; 4] = [Self::Name, Self::Files, Self::Size, Self::Modified];

    const fn as_str(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Files => "files",
            Self::Size => "size",
            Self::Modified => "modified",
        }
    }

    /// Names read A to Z; counts, sizes and dates read largest or newest first.
    const fn descending_by_default(self) -> bool {
        !matches!(self, Self::Name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub key: SortKey,
    pub descending: bool,
}

impl Default for Sort {
    fn default() -> Self {
        Self {
            key: SortKey::Name,
            descending: false,
        }
    }
}

impl Sort {
    pub fn parse(query: &ListingQuery) -> Result<Self, String> {
        let key = match query.sort.as_deref() {
            None => SortKey::Name,
            Some(value) => SortKey::ALL
                .into_iter()
                .find(|key| key.as_str() == value)
                .ok_or_else(|| {
                    "error: sort must be one of name, files, size, modified\n".to_string()
                })?,
        };
        let descending = match query.order.as_deref() {
            None => key.descending_by_default(),
            Some("asc") => false,
            Some("desc") => true,
            Some(_) => return Err("error: order must be asc or desc\n".to_string()),
        };
        Ok(Self { key, descending })
    }

    fn order<T: Ord>(self, left: &T, right: &T) -> std::cmp::Ordering {
        let ordering = left.cmp(right);
        if self.descending {
            ordering.reverse()
        } else {
            ordering
        }
    }

    /// The query string of a column header: this column in its natural
    /// direction, or the opposite direction when it is already the sort.
    fn header_query(self, key: SortKey) -> String {
        let descending = if self.key == key {
            !self.descending
        } else {
            key.descending_by_default()
        };
        let order = if descending { "desc" } else { "asc" };
        format!("?sort={}&order={order}", key.as_str())
    }
}

/// The optional TSV columns a request asked for with `columns=`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Columns {
    pub modified: bool,
}

impl Columns {
    pub fn parse(query: &ListingQuery) -> Result<Self, String> {
        let mut columns = Self::default();
        for name in query
            .columns
            .as_deref()
            .unwrap_or("")
            .split(',')
            .filter(|name| !name.is_empty())
        {
            match name {
                "modified" => columns.modified = true,
                _ => {
                    return Err(format!(
                        "error: unknown column {name:?}; optional columns: {}\n",
                        OPTIONAL_COLUMNS.join(", ")
                    ));
                }
            }
        }
        Ok(columns)
    }

    fn header(self, base: &str) -> String {
        let mut header = base.to_string();
        if self.modified {
            header.push_str("\tmodified");
        }
        header
    }

    fn push(self, row: &mut String, modified: Option<i64>) {
        if self.modified {
            row.push('\t');
            if let Some(modified) = modified.and_then(known_time) {
                row.push_str(&rfc3339(modified));
            }
        }
    }
}

/// How a listing was asked to be shown: its order and, for TSV, which page
/// and optional columns.
#[derive(Debug, Clone, Copy, Default)]
pub struct ListingView<'a> {
    pub sort: Sort,
    pub tsv: Option<TsvRequest<'a>>,
}

/// How a TSV listing was asked for: where it lives, which page, which
/// optional columns.
#[derive(Debug, Clone, Copy)]
pub struct TsvRequest<'a> {
    pub uri: &'a Uri,
    pub paging: Paging,
    pub columns: Columns,
}

impl<'a> TsvRequest<'a> {
    pub fn parse(uri: &'a Uri, query: &ListingQuery) -> Result<Self, String> {
        Ok(Self {
            uri,
            paging: Paging::parse(query)?,
            columns: Columns::parse(query)?,
        })
    }
}

/// Orders a directory listing in place: folders, then files, then aliases,
/// each by `sort` and then by name.
pub fn sort_listing(rel: &str, list: &mut DirList, sort: Sort) {
    fn name<'a>(rel: &str, alias: &'a AliasEntry) -> &'a str {
        direct_alias_name(rel, &alias.path).unwrap_or(&alias.path)
    }
    list.entries.sort_by(|left, right| {
        let group = |entry: &DirEnt| u8::from(entry.kind == EntryKind::File);
        group(left)
            .cmp(&group(right))
            .then_with(|| match sort.key {
                SortKey::Name => sort.order(&left.name, &right.name),
                SortKey::Files => sort.order(&left.files, &right.files),
                SortKey::Size => sort.order(&left.bytes, &right.bytes),
                SortKey::Modified => sort.order(&left.modified, &right.modified),
            })
            .then_with(|| left.name.cmp(&right.name))
    });
    list.aliases.sort_by(|left, right| {
        match sort.key {
            SortKey::Name => sort.order(&name(rel, left), &name(rel, right)),
            SortKey::Files => sort.order(&left.resolved_files, &right.resolved_files),
            SortKey::Size => sort.order(&left.resolved_size, &right.resolved_size),
            SortKey::Modified => sort.order(&left.modified, &right.modified),
        }
        .then_with(|| name(rel, left).cmp(name(rel, right)))
    });
}

/// Orders the site list in place. Sites have no folders or aliases, so the key
/// alone decides.
pub fn sort_sites(list: &mut SiteList, sort: Sort) {
    list.entries.sort_by(|left, right| {
        match sort.key {
            SortKey::Name => sort.order(&left.name, &right.name),
            SortKey::Files => sort.order(&left.files, &right.files),
            SortKey::Size => sort.order(&left.bytes, &right.bytes),
            SortKey::Modified => sort.order(&left.modified, &right.modified),
        }
        .then_with(|| left.name.cmp(&right.name))
    });
}

/// `None` for the zero an entry can only carry if it was never stamped.
const fn known_time(millis: i64) -> Option<i64> {
    if millis > 0 { Some(millis) } else { None }
}

fn utc(millis: i64) -> time::OffsetDateTime {
    time::OffsetDateTime::from_unix_timestamp(millis.div_euclid(1000))
        .unwrap_or(time::OffsetDateTime::UNIX_EPOCH)
}

/// `2026-10-06T14:03:12Z`: whole seconds, UTC.
fn rfc3339(millis: i64) -> String {
    let time = utc(millis);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        time.year(),
        u8::from(time.month()),
        time.day(),
        time.hour(),
        time.minute(),
        time.second()
    )
}

/// `2026-10-06 14:03Z`: what listings show a person. The `Z` marks UTC; the
/// HTML view swaps in the reader's local time, without it.
fn date_label(millis: i64) -> String {
    let time = utc(millis);
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}Z",
        time.year(),
        u8::from(time.month()),
        time.day(),
        time.hour(),
        time.minute()
    )
}

/// The width of [`date_label`]'s output.
const DATE_WIDTH: usize = "2026-10-06 14:03Z".len();

impl ListingQuery {
    /// `?recursive` asks for the whole inventory rather than one directory.
    pub fn recursive(&self) -> bool {
        self.recursive
            .as_deref()
            .is_some_and(|value| !matches!(value, "0" | "false" | "no"))
    }
}

/// Which slice of a listing to return. `limit: None` is everything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Paging {
    pub limit: Option<usize>,
    pub page: usize,
}

impl Paging {
    pub fn parse(query: &ListingQuery) -> Result<Self, String> {
        let number = |name: &str, value: &str| -> Result<usize, String> {
            value
                .parse::<usize>()
                .ok()
                .filter(|value| *value >= 1)
                .ok_or_else(|| format!("error: {name} must be a whole number of at least 1\n"))
        };
        let limit = query
            .limit
            .as_deref()
            .map(|value| number("limit", value))
            .transpose()?;
        if limit.is_some_and(|limit| limit > MAX_PAGE_LIMIT) {
            return Err(format!("error: limit must be at most {MAX_PAGE_LIMIT}\n"));
        }
        let page = query
            .page
            .as_deref()
            .map(|value| number("page", value))
            .transpose()?;
        if page.is_some() && limit.is_none() {
            return Err("error: page needs a limit\n".to_string());
        }
        Ok(Self {
            limit,
            page: page.unwrap_or(1),
        })
    }

    fn slice<T>(self, rows: &[T]) -> &[T] {
        let Some(limit) = self.limit else {
            return rows;
        };
        let start = (self.page - 1).saturating_mul(limit).min(rows.len());
        &rows[start..(start + limit).min(rows.len())]
    }
}

/// A listing's rows as a TSV body, with the total before paging.
pub struct TsvPage {
    pub body: String,
    pub total: usize,
}

fn tsv_page(header: &str, rows: &[String], paging: Paging) -> TsvPage {
    let page = paging.slice(rows);
    let mut body = String::with_capacity(
        header.len() + 1 + page.iter().map(|row| row.len() + 1).sum::<usize>(),
    );
    body.push_str(header);
    body.push('\n');
    for row in page {
        body.push_str(row);
        body.push('\n');
    }
    TsvPage {
        body,
        total: rows.len(),
    }
}

/// A site's whole inventory: files, then aliases, each sorted by path.
///
/// `kind` is `file` or `alias`; `size` is bytes (empty for a dangling alias);
/// `value` is a file's content hash or an alias's target.
pub fn inventory_tsv(inventory: &symbol_contract::SiteInventory, paging: Paging) -> TsvPage {
    let mut rows = Vec::with_capacity(inventory.files.len() + inventory.aliases.len());
    rows.extend(
        inventory
            .files
            .iter()
            .map(|file| format!("file\t{}\t{}\t{}", file.size, file.hash, file.path)),
    );
    rows.extend(inventory.aliases.iter().map(|alias| {
        let size = alias.size.map(|size| size.to_string()).unwrap_or_default();
        format!("alias\t{size}\t{}\t{}", alias.target, alias.path)
    }));
    tsv_page(INVENTORY_HEADER, &rows, paging)
}

/// One directory: subdirectories, then files, then aliases, in the order
/// [`sort_listing`] left them.
///
/// `files` is a directory's (or directory alias's) file count, `bytes` its
/// size; `target` is set for aliases only. `columns=modified` appends when
/// each entry's content last changed.
pub fn listing_tsv(rel: &str, list: &DirList, paging: Paging, columns: Columns) -> TsvPage {
    let mut rows = Vec::with_capacity(list.entries.len() + list.aliases.len());
    rows.extend(list.entries.iter().map(|entry| {
        let mut row = match entry.kind {
            EntryKind::Directory => format!(
                "directory\t{}\t{}\t{}\t",
                entry.files, entry.bytes, entry.name
            ),
            EntryKind::File => format!("file\t\t{}\t{}\t", entry.bytes, entry.name),
        };
        columns.push(&mut row, Some(entry.modified));
        row
    }));
    rows.extend(list.aliases.iter().filter_map(|alias| {
        direct_alias_name(rel, &alias.path).map(|name| {
            let files = alias
                .resolved_files
                .map(|files| files.to_string())
                .unwrap_or_default();
            let bytes = alias
                .resolved_size
                .map(|bytes| bytes.to_string())
                .unwrap_or_default();
            let mut row = format!(
                "alias\t{files}\t{bytes}\t{name}\t{}",
                alias.canonical_target
            );
            columns.push(&mut row, Some(alias.modified));
            row
        })
    }));
    tsv_page(&columns.header(LISTING_HEADER), &rows, paging)
}

fn sites_tsv(list: &SiteList, paging: Paging, columns: Columns) -> TsvPage {
    let mut rows = Vec::with_capacity(list.entries.len() + 1);
    let mut builtin = "builtin\t\t0\tAPI\t".to_string();
    columns.push(&mut builtin, None);
    rows.push(builtin);
    rows.extend(list.entries.iter().map(|entry| {
        let mut row = format!("site\t{}\t{}\t{}\t", entry.files, entry.bytes, entry.name);
        columns.push(&mut row, Some(entry.modified));
        row
    }));
    tsv_page(&columns.header(LISTING_HEADER), &rows, paging)
}

/// Adds `Entry-Count` and, when paged, `Link` to the neighbouring pages.
///
/// The links repeat the request's path and every other query parameter, so
/// following `next` keeps `recursive` and the limit.
pub fn add_paging_headers(response: &mut Response, uri: &Uri, paging: Paging, total: usize) {
    let headers = response.headers_mut();
    headers.insert(
        "entry-count",
        HeaderValue::from_str(&total.to_string()).expect("valid count"),
    );
    let Some(limit) = paging.limit else {
        return;
    };
    let pages = total.div_ceil(limit).max(1);
    let kept: Vec<&str> = uri
        .query()
        .unwrap_or("")
        .split('&')
        .filter(|pair| !pair.is_empty() && !pair.starts_with("page=") && *pair != "page")
        .collect();
    let link = |page: usize, rel: &str| {
        let mut query = kept.join("&");
        if !query.is_empty() {
            query.push('&');
        }
        format!("<{}?{query}page={page}>; rel=\"{rel}\"", uri.path())
    };
    let mut links = Vec::new();
    if paging.page > 1 {
        links.push(link((paging.page - 1).min(pages), "prev"));
    }
    if paging.page < pages {
        links.push(link(paging.page + 1, "next"));
    }
    if !links.is_empty()
        && let Ok(value) = HeaderValue::from_str(&links.join(", "))
    {
        headers.insert(header::LINK, value);
    }
}

pub fn tsv_response(headers: &HeaderMap, uri: &Uri, page: TsvPage, paging: Paging) -> Response {
    let total = page.total;
    let mut response = cached_response(headers, page.body, TSV_TYPE);
    if response.status() == axum::http::StatusCode::OK {
        add_paging_headers(&mut response, uri, paging, total);
    }
    response
}

pub fn bad_query(message: String) -> Response {
    let mut response = Response::new(axum::body::Body::from(message));
    *response.status_mut() = axum::http::StatusCode::BAD_REQUEST;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    response
}

fn accepts(headers: &HeaderMap, media_type: &str) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|accept| {
            accept
                .split(',')
                .any(|part| part.trim().split(';').next().map(str::trim) == Some(media_type))
        })
}

fn wants_json(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|accept| {
            accept
                .split(',')
                .any(|part| part.trim().split(';').next() == Some("application/json"))
        })
}

fn json_response(headers: &HeaderMap, value: &Listing) -> Response {
    let body = serde_json::to_vec(&value).expect("listing serializes");
    cached_response(headers, Bytes::from(body), "application/json")
}

fn cached_response(
    headers: &HeaderMap,
    body: impl Into<Bytes>,
    content_type: &'static str,
) -> Response {
    let mut representation = Representation::new(body, content_type);
    representation.vary = Some(HeaderValue::from_static("Accept, User-Agent"));
    http_cache::respond(headers, representation)
}

/// One line of a plain-text listing. `files` is blank for a file, `modified`
/// for a total, and `suffix` follows the size, as in `0 B built-in`.
#[derive(Default)]
struct PlainRow<'a> {
    name: &'a str,
    files: Option<u64>,
    bytes: u64,
    modified: Option<i64>,
    suffix: &'a str,
}

impl PlainRow<'_> {
    fn push(&self, out: &mut String, layout: ListingLayout) {
        let count = self.files.map_or_else(
            || " ".repeat(layout.count + 6),
            |files| format!("{files:>width$} files", width = layout.count),
        );
        let date = self
            .modified
            .and_then(known_time)
            .map_or_else(String::new, |modified| {
                format!("   {:>DATE_WIDTH$}", date_label(modified))
            });
        writeln!(
            out,
            "{:<width$} {count}   {}{date}{}",
            self.name,
            HumanSize::new(self.bytes).aligned(layout.size),
            self.suffix,
            width = layout.name,
        )
        .unwrap();
    }
}

fn parent_href(site: &str, rel: &str, files_view: bool) -> Option<String> {
    if rel.is_empty() {
        return if files_view {
            Some("/FILES".to_string())
        } else {
            None
        };
    }
    let parent = match rel.rsplit_once('/') {
        Some((p, _)) => p,
        None => "",
    };
    Some(dir_href(site, parent, files_view))
}

fn dir_href(site: &str, rel: &str, files_view: bool) -> String {
    if files_view {
        if rel.is_empty() {
            format!("/{site}/FILES/")
        } else {
            format!("/{site}/FILES/{rel}/")
        }
    } else if rel.is_empty() {
        format!("/{site}/")
    } else {
        format!("/{site}/{rel}/")
    }
}

fn listing_occupied_names<'a>(rel: &str, list: &'a DirList) -> HashSet<&'a str> {
    let mut names = HashSet::new();
    for entry in &list.entries {
        names.insert(entry.name.as_str());
    }
    for alias in &list.aliases {
        if let Some(name) = direct_alias_name(rel, &alias.path) {
            names.insert(name);
        }
    }
    names
}

/// Whether `name` is the file that the containing directory's URL serves.
///
/// `find_index` prefers `index.html`, so `index.htm` only stands in for the
/// directory when there is no `index.html` beside it.
fn serves_directory_index(name: &str, occupied: &HashSet<&str>) -> bool {
    match name {
        "index.html" => true,
        "index.htm" => !occupied.contains("index.html"),
        _ => false,
    }
}

fn join_rel(rel: &str, name: &str) -> String {
    if rel.is_empty() {
        name.to_string()
    } else {
        format!("{rel}/{name}")
    }
}

/// Builds the link target for one listing row.
///
/// Dropping an `.html` suffix is a URL-cleaning concern and nothing else, so it
/// happens here and never to the visible label. An index file links to the
/// directory that serves it rather than to a path that would only redirect.
fn entry_href(
    site: &str,
    rel: &str,
    name: &str,
    kind: EntryKind,
    files_view: bool,
    occupied: &HashSet<&str>,
) -> String {
    match kind {
        EntryKind::Directory => dir_href(site, &join_rel(rel, name), files_view),
        EntryKind::File if serves_directory_index(name, occupied) => dir_href(site, rel, false),
        EntryKind::File => {
            let target = pretty_html_name(name, |candidate| occupied.contains(candidate));
            format!("/{site}/{}", join_rel(rel, target))
        }
    }
}

fn size_label(n: u64) -> String {
    let size = HumanSize::new(n);
    if size.fraction.is_empty() {
        format!("{} {}", size.integer, size.unit)
    } else {
        format!("{}.{} {}", size.integer, size.fraction, size.unit)
    }
}

#[derive(Clone)]
struct HumanSize {
    integer: String,
    fraction: String,
    unit: &'static str,
}

impl HumanSize {
    fn new(bytes: u64) -> Self {
        let (divisor, unit) = if bytes < 1024 {
            (1, "B")
        } else if bytes < 1024 * 1024 {
            (1024, "KiB")
        } else if bytes < 1024 * 1024 * 1024 {
            (1024 * 1024, "MiB")
        } else if bytes < 1024_u64.pow(4) {
            (1024_u64.pow(3), "GiB")
        } else {
            (1024_u64.pow(4), "TiB")
        };
        let precision = if divisor == 1 || bytes >= divisor * 100 {
            0
        } else if bytes >= divisor * 10 {
            1
        } else {
            2
        };
        let scale = match precision {
            0 => 1,
            1 => 10,
            2 => 100,
            _ => unreachable!(),
        };
        let divisor = u128::from(divisor);
        let rounded = (u128::from(bytes) * scale + divisor / 2) / divisor;
        Self {
            integer: (rounded / scale).to_string(),
            fraction: if precision == 0 {
                String::new()
            } else {
                format!("{:0width$}", rounded % scale, width = precision)
            },
            unit,
        }
    }

    fn aligned(&self, layout: SizeLayout) -> String {
        let mut number = format!("{:>width$}", self.integer, width = layout.integer);
        if layout.fraction > 0 {
            if self.fraction.is_empty() {
                number.push_str(&" ".repeat(layout.fraction + 1));
            } else {
                number.push('.');
                number.push_str(&self.fraction);
                number.push_str(&" ".repeat(layout.fraction - self.fraction.len()));
            }
        }
        format!("{number} {:<width$}", self.unit, width = layout.unit)
    }
}

#[derive(Clone, Copy)]
struct SizeLayout {
    integer: usize,
    fraction: usize,
    unit: usize,
}

#[derive(Clone, Copy)]
struct ListingLayout {
    name: usize,
    count: usize,
    size: SizeLayout,
}

impl SizeLayout {
    fn from_sizes(sizes: &[HumanSize]) -> Self {
        Self {
            integer: sizes
                .iter()
                .map(|size| size.integer.len())
                .max()
                .unwrap_or(1),
            fraction: sizes
                .iter()
                .map(|size| size.fraction.len())
                .max()
                .unwrap_or(0),
            unit: sizes.iter().map(|size| size.unit.len()).max().unwrap_or(1),
        }
    }
}

const fn digits(n: u64) -> usize {
    if n == 0 { 1 } else { n.ilog10() as usize + 1 }
}

const BASE_STYLE: &str = static_asset!("base.css");
const STYLE: &str = static_asset!("browse.css");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SiteEnt;
    use axum::body::to_bytes;
    use axum::extract::Query;
    use axum::http::{HeaderValue, StatusCode};

    fn accept(value: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_static(value));
        headers
    }

    async fn body_text(response: Response) -> String {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    /// 2026-10-06T14:03:12Z.
    const OCT_6: i64 = 1_791_295_392_000;
    /// 2026-09-30T08:15:00Z.
    const SEP_30: i64 = 1_790_756_100_000;
    /// 2025-01-02T03:04:05Z.
    const JAN_2: i64 = 1_735_787_045_000;

    #[test]
    fn plain_site_sizes_align_ones_places_and_units() {
        let list = SiteList {
            files: 11,
            alias_count: 0,
            bytes: 4_194_304,
            entries: vec![
                SiteEnt {
                    name: "hello".to_string(),
                    files: 8,
                    bytes: 3_774_464,
                    modified: OCT_6,
                },
                SiteEnt {
                    name: "notes".to_string(),
                    files: 3,
                    bytes: 419_840,
                    modified: SEP_30,
                },
            ],
        };
        assert_eq!(
            render_sites_plain(&list),
            concat!(
                "API/                      0    B   built-in\n",
                "hello/        8 files     3.60 MiB   2026-10-06 14:03Z\n",
                "notes/        3 files   410    KiB   2026-09-30 08:15Z\n",
                "             11 files     4.00 MiB total\n",
            )
        );
    }

    #[test]
    fn timestamps_render_as_whole_utc_seconds_and_minutes() {
        assert_eq!(rfc3339(OCT_6 + 999), "2026-10-06T14:03:12Z");
        assert_eq!(date_label(OCT_6), "2026-10-06 14:03Z");
        assert_eq!(date_label(OCT_6).len(), DATE_WIDTH);
        assert_eq!(known_time(0), None);
        assert_eq!(known_time(JAN_2), Some(JAN_2));
    }

    #[test]
    fn plain_directory_sizes_align_files_and_directories() {
        let list = DirList {
            files: 8,
            bytes: 3_774_464,
            alias_count: 0,
            aliases: Vec::new(),
            entries: vec![
                DirEnt {
                    kind: EntryKind::Directory,
                    name: "assets".to_string(),
                    files: 5,
                    bytes: 3_565_158,
                    modified: OCT_6,
                },
                DirEnt {
                    kind: EntryKind::Directory,
                    name: "css".to_string(),
                    files: 2,
                    bytes: 188_743,
                    modified: SEP_30,
                },
                DirEnt {
                    kind: EntryKind::File,
                    name: "index.html".to_string(),
                    files: 1,
                    bytes: 20_563,
                    modified: JAN_2,
                },
            ],
        };
        assert_eq!(
            render_plain("hello", "", &list),
            "hello/        8 files     3.60 MiB\n\
             assets/       5 files     3.40 MiB   2026-10-06 14:03Z\n\
             css/          2 files   184    KiB   2026-09-30 08:15Z\n\
             index.html               20.1  KiB   2025-01-02 03:04Z\n"
        );
    }

    fn query(pairs: &str) -> ListingQuery {
        Query::<ListingQuery>::try_from_uri(&format!("/x?{pairs}").parse::<Uri>().unwrap())
            .unwrap()
            .0
    }

    fn sample_list() -> DirList {
        let mut list = dir_list(vec![
            DirEnt {
                modified: SEP_30,
                bytes: 4096,
                ..file_entry("b.txt")
            },
            DirEnt {
                modified: OCT_6,
                bytes: 10,
                ..file_entry("a.txt")
            },
            DirEnt {
                modified: JAN_2,
                files: 7,
                bytes: 1,
                ..dir_entry("zeta")
            },
            DirEnt {
                modified: OCT_6,
                files: 2,
                bytes: 9000,
                ..dir_entry("alpha")
            },
        ]);
        list.aliases = vec![
            AliasEntry {
                modified: JAN_2,
                ..alias_entry("new", "a.txt")
            },
            AliasEntry {
                modified: OCT_6,
                ..alias_entry("latest", "b.txt")
            },
        ];
        list
    }

    fn order(list: &DirList) -> Vec<&str> {
        list.entries
            .iter()
            .map(|entry| entry.name.as_str())
            .chain(list.aliases.iter().map(|alias| alias.path.as_str()))
            .collect()
    }

    #[test]
    fn sorting_keeps_folders_first_and_aliases_last() {
        let mut list = sample_list();
        sort_listing("", &mut list, Sort::default());
        assert_eq!(
            order(&list),
            ["alpha", "zeta", "a.txt", "b.txt", "latest", "new"]
        );

        let newest = Sort::parse(&query("sort=modified")).unwrap();
        assert!(newest.descending, "dates read newest first by default");
        sort_listing("", &mut list, newest);
        assert_eq!(
            order(&list),
            ["alpha", "zeta", "a.txt", "b.txt", "latest", "new"]
        );

        sort_listing(
            "",
            &mut list,
            Sort::parse(&query("sort=size&order=asc")).unwrap(),
        );
        assert_eq!(
            order(&list),
            ["zeta", "alpha", "a.txt", "b.txt", "latest", "new"]
        );

        sort_listing("", &mut list, Sort::parse(&query("sort=files")).unwrap());
        assert_eq!(order(&list)[..2], ["zeta", "alpha"]);

        sort_listing("", &mut list, Sort::parse(&query("order=desc")).unwrap());
        assert_eq!(
            order(&list),
            ["zeta", "alpha", "b.txt", "a.txt", "new", "latest"]
        );
    }

    #[test]
    fn sort_parameters_are_validated() {
        assert_eq!(Sort::parse(&query("")).unwrap(), Sort::default());
        assert!(Sort::parse(&query("sort=date")).is_err());
        assert!(Sort::parse(&query("order=up")).is_err());
        assert_eq!(Columns::parse(&query("")).unwrap(), Columns::default());
        assert!(Columns::parse(&query("columns=modified")).unwrap().modified);
        assert!(Columns::parse(&query("columns=hash")).is_err());
    }

    #[test]
    fn headers_link_to_their_sort_and_reverse_the_active_one() {
        let sort = Sort::default();
        assert_eq!(sort.header_query(SortKey::Name), "?sort=name&order=desc");
        assert_eq!(sort.header_query(SortKey::Size), "?sort=size&order=desc");
        let newest = Sort {
            key: SortKey::Modified,
            descending: true,
        };
        assert_eq!(
            newest.header_query(SortKey::Modified),
            "?sort=modified&order=asc"
        );
        assert_eq!(newest.header_query(SortKey::Name), "?sort=name&order=asc");

        let body = render_html("hello", "", &sample_list(), true, newest);
        assert!(
            body.contains(r#"href="?sort=modified&amp;order=asc" aria-sort="descending""#),
            "{body}"
        );
        assert!(
            body.contains(r#"<time datetime="2026-10-06T14:03:12Z""#),
            "{body}"
        );
        assert!(body.contains(">2026-10-06 14:03Z</time>"), "{body}");
    }

    #[test]
    fn tsv_listings_append_modified_only_when_asked() {
        let list = sample_list();
        let plain = listing_tsv(
            "",
            &list,
            Paging::parse(&query("")).unwrap(),
            Columns::default(),
        );
        assert!(plain.body.starts_with("kind\tfiles\tbytes\tname\ttarget\n"));
        assert!(!plain.body.contains("2026"));

        let dated = listing_tsv(
            "",
            &list,
            Paging::parse(&query("")).unwrap(),
            Columns { modified: true },
        );
        let lines: Vec<&str> = dated.body.lines().collect();
        assert_eq!(lines[0], "kind\tfiles\tbytes\tname\ttarget\tmodified");
        assert!(lines.contains(&"file\t\t4096\tb.txt\t\t2026-09-30T08:15:00Z"));
        assert!(lines.contains(&"directory\t7\t1\tzeta\t\t2025-01-02T03:04:05Z"));
        assert!(lines.contains(&"alias\t\t\tlatest\tb.txt\t2026-10-06T14:03:12Z"));

        let sites = SiteList {
            files: 1,
            alias_count: 0,
            bytes: 1,
            entries: vec![SiteEnt {
                name: "hello".to_string(),
                files: 1,
                bytes: 1,
                modified: OCT_6,
            }],
        };
        let body = sites_tsv(
            &sites,
            Paging::parse(&query("")).unwrap(),
            Columns { modified: true },
        )
        .body;
        assert_eq!(
            body,
            "kind\tfiles\tbytes\tname\ttarget\tmodified\n\
             builtin\t\t0\tAPI\t\t\n\
             site\t1\t1\thello\t\t2026-10-06T14:03:12Z\n"
        );
    }

    #[tokio::test]
    async fn files_responses_negotiate_json_and_html() {
        let list = SiteList {
            files: 1,
            alias_count: 0,
            bytes: 512,
            entries: vec![SiteEnt {
                name: "hello".to_string(),
                files: 1,
                bytes: 512,
                modified: OCT_6,
            }],
        };
        let response = sites(
            &accept("application/json"),
            &Uri::from_static("/FILES"),
            &ListingQuery::default(),
            list.clone(),
        );
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        assert_eq!(
            body_text(response).await,
            r#"{"path":"/","files":1,"aliases":0,"bytes":512,"entries":[{"kind":"builtin","name":"API","files":null,"bytes":0},{"kind":"site","name":"hello","files":1,"bytes":512}]}"#
        );

        let response = sites(
            &accept("text/html"),
            &Uri::from_static("/FILES"),
            &ListingQuery::default(),
            list,
        );
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        assert!(body.contains("1 files · 512 B"));
        assert!(body.contains(">512 B</span>"), "{body}");
        assert!(body.contains(">2026-10-06 14:03Z</time>"), "{body}");
        assert!(body.contains(r#"href="/API/""#));
        assert!(body.contains(r#"href="/hello/FILES/""#));
        assert!(body.contains("font-variant-numeric: tabular-nums"));
    }

    #[test]
    fn listing_etag_revalidates_and_changes_with_content() {
        let mut list = SiteList {
            files: 1,
            alias_count: 0,
            bytes: 5,
            entries: vec![SiteEnt {
                name: "hello".to_string(),
                files: 1,
                bytes: 5,
                modified: OCT_6,
            }],
        };
        let response = sites(
            &HeaderMap::new(),
            &Uri::from_static("/FILES"),
            &ListingQuery::default(),
            list.clone(),
        );
        let etag = response.headers()[header::ETAG].clone();

        let mut conditional = HeaderMap::new();
        conditional.insert(header::IF_NONE_MATCH, etag.clone());
        let response = sites(
            &conditional,
            &Uri::from_static("/FILES"),
            &ListingQuery::default(),
            list.clone(),
        );
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);

        list.bytes += 1;
        list.entries[0].bytes += 1;
        let response = sites(
            &conditional,
            &Uri::from_static("/FILES"),
            &ListingQuery::default(),
            list,
        );
        assert_eq!(response.status(), StatusCode::OK);
        assert_ne!(response.headers()[header::ETAG], etag);
    }

    #[test]
    fn json_accept_detection_is_explicit() {
        assert!(wants_json(&accept("application/json; charset=utf-8")));
        assert!(!wants_json(&accept("*/*")));
    }

    fn dir_entry(name: &str) -> DirEnt {
        DirEnt {
            kind: EntryKind::Directory,
            name: name.to_string(),
            files: 1,
            bytes: 512,
            modified: OCT_6,
        }
    }

    fn file_entry(name: &str) -> DirEnt {
        DirEnt {
            kind: EntryKind::File,
            name: name.to_string(),
            files: 1,
            bytes: 512,
            modified: OCT_6,
        }
    }

    fn alias_entry(path: &str, target: &str) -> AliasEntry {
        AliasEntry {
            path: path.to_string(),
            canonical_target: target.to_string(),
            resolved_kind: None,
            resolved_hash: None,
            resolved_size: None,
            resolved_files: None,
            modified: OCT_6,
        }
    }

    fn dir_list(entries: Vec<DirEnt>) -> DirList {
        DirList {
            files: entries.len() as u64,
            bytes: 512 * entries.len() as u64,
            alias_count: 0,
            aliases: Vec::new(),
            entries,
        }
    }

    #[test]
    fn files_navigation_keeps_directory_links_in_files_view() {
        assert_eq!(parent_href("hello", "", true).as_deref(), Some("/FILES"));
        assert_eq!(
            parent_href("hello", "assets/css", true).as_deref(),
            Some("/hello/FILES/assets/")
        );
        assert_eq!(
            entry_href(
                "hello",
                "assets",
                "css",
                EntryKind::Directory,
                true,
                &HashSet::new()
            ),
            "/hello/FILES/assets/css/"
        );
    }

    #[test]
    fn listing_labels_keep_the_stored_extension_while_links_stay_pretty() {
        let list = dir_list(vec![file_entry("about.html"), file_entry("style.css")]);

        for files_view in [true, false] {
            let body = render_html("hello", "", &list, files_view, Sort::default());
            assert!(
                body.contains(">about.html</span>"),
                "label lost its extension in files_view={files_view}: {body}"
            );
            assert!(body.contains(r#"href="/hello/about""#), "{body}");
            assert!(!body.contains(r#"href="/hello/about.html""#), "{body}");
            assert!(body.contains(">style.css</span>"), "{body}");
            assert!(body.contains(r#"href="/hello/style.css""#), "{body}");
        }
    }

    #[test]
    fn an_occupied_stem_keeps_the_extension_in_both_label_and_link() {
        let list = dir_list(vec![file_entry("about.html"), dir_entry("about")]);
        let body = render_html("hello", "", &list, true, Sort::default());
        assert!(body.contains(">about.html</span>"), "{body}");
        assert!(body.contains(r#"href="/hello/about.html""#), "{body}");
    }

    #[test]
    fn index_entries_link_to_the_directory_that_serves_them() {
        let list = dir_list(vec![file_entry("index.html")]);
        let body = render_html("hello", "docs", &list, true, Sort::default());
        assert!(body.contains(r#"class="see-site" href="/hello/docs/""#));
        assert!(body.contains(">see site</a>"));
        assert!(body.contains(">index.html</span>"), "{body}");
        assert!(body.contains(r#"href="/hello/docs/""#), "{body}");
        assert!(!body.contains(r#"href="/hello/docs/index""#), "{body}");

        let root = dir_list(vec![file_entry("index.html")]);
        let body = render_html("hello", "", &root, true, Sort::default());
        assert!(body.contains(r#"href="/hello/""#), "{body}");
        assert!(!body.contains(r#"href="/hello/index""#), "{body}");
    }

    #[test]
    fn only_the_index_that_wins_links_to_the_directory() {
        let mut occupied = HashSet::new();
        occupied.insert("index.htm");
        assert!(serves_directory_index("index.html", &occupied));
        assert!(serves_directory_index("index.htm", &occupied));

        // `find_index` prefers `index.html`, so `index.htm` must stay put.
        occupied.insert("index.html");
        assert!(serves_directory_index("index.html", &occupied));
        assert!(!serves_directory_index("index.htm", &occupied));
        assert!(!serves_directory_index("about.html", &occupied));

        let list = dir_list(vec![file_entry("index.html"), file_entry("index.htm")]);
        let body = render_html("hello", "docs", &list, true, Sort::default());
        assert!(body.contains(r#"href="/hello/docs/index.htm""#), "{body}");
        assert!(body.contains(">index.htm</span>"), "{body}");
    }

    #[tokio::test]
    async fn aliases_render_as_arrows_and_typed_nullable_json_entries() {
        let list = DirList {
            files: 0,
            bytes: 0,
            alias_count: 1,
            aliases: vec![alias_entry("latest", "releases/current")],
            entries: Vec::new(),
        };
        assert!(
            render_plain("hello", "", &list)
                .contains("latest -> releases/current   2026-10-06 14:03Z")
        );
        assert!(
            render_html("hello", "", &list, true, Sort::default())
                .contains("latest -&gt; releases/current")
        );

        let response = listing(
            &accept("application/json"),
            "hello",
            "",
            list,
            true,
            ListingView::default(),
        );
        let value: serde_json::Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(value["aliases"], 1);
        assert_eq!(value["entries"][0]["kind"], "alias");
        assert_eq!(value["entries"][0]["target"], "releases/current");
        assert_eq!(value["entries"][0]["target_kind"], serde_json::Value::Null);
        assert_eq!(value["entries"][0]["files"], serde_json::Value::Null);
        assert_eq!(value["entries"][0]["bytes"], serde_json::Value::Null);
    }
}
