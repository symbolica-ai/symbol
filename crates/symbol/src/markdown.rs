//! Browser rendering for user-hosted Markdown.
//!
//! A `.md` file is served as HTML only when a request explicitly prefers HTML,
//! which a browser's top-level navigation does and `curl`, `fetch()` and the
//! SDKs do not. Everything else, and every `/RAW` request, gets the source.
//!
//! The rendered page is meant to be restyled, so it looks nothing like the
//! guide at `/`. Front matter in YAML (`---`) or TOML (`+++`) sets the title,
//! theme, font and width defaults, adds CSS, stylesheets, scripts or raw `<head>`
//! HTML, and toggles features. Raw HTML in the body passes through untouched:
//! authors can already host arbitrary HTML on this service, so the renderer has
//! nothing to protect by restricting it, and flexibility is the point.

use std::collections::HashMap;
use std::fmt::Write as _;

use axum::http::{HeaderMap, header};
use maud::{DOCTYPE, Markup, PreEscaped, html};
use pulldown_cmark::{CodeBlockKind, CowStr, Event, Options, Parser, Tag, TagEnd};
use serde_json::{Map, Value};

/// Files above this are always served raw: rendering holds the whole source
/// and its output in memory, and nobody reads a 4 MiB document in a browser.
pub const RENDER_LIMIT_BYTES: u64 = 4 * 1024 * 1024;

const THEMES: [&str; 4] = ["auto", "light", "dark", "sepia"];
const FONTS: [&str; 3] = ["sans", "serif", "mono"];
const WIDTHS: [&str; 3] = ["narrow", "wide", "full"];
const CONTROLS: [&str; 5] = ["theme", "font", "width", "size", "raw"];

/// Whether `path` names a Markdown file.
pub fn is_markdown(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    name.rsplit_once('.').is_some_and(|(_, extension)| {
        extension.eq_ignore_ascii_case("md") || extension.eq_ignore_ascii_case("markdown")
    })
}

/// Whether a request asked for a document rather than the file.
///
/// Rendering keys on an explicit preference for HTML in `Accept`. A browser's
/// top-level navigation sends one; `fetch()` and curl send `*/*`, so a site
/// that fetches its own Markdown to render client-side keeps receiving the
/// source. User-Agent sniffing is deliberately not used: it would hand HTML to
/// a browser `fetch()` that never asked for it.
pub fn wants_rendering(headers: &HeaderMap) -> bool {
    let Some(accept) = headers
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
    else {
        return false;
    };
    let mut html_q: Option<f32> = None;
    let mut source_q: Option<f32> = None;
    for part in accept.split(',') {
        let mut pieces = part.split(';');
        let media = pieces
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        let mut quality = 1.0_f32;
        for parameter in pieces {
            let parameter = parameter.trim().to_ascii_lowercase();
            if let Some(value) = parameter.strip_prefix("q=") {
                quality = value.trim().parse().unwrap_or(0.0);
            }
        }
        let slot = match media.as_str() {
            "text/html" | "application/xhtml+xml" => &mut html_q,
            "text/markdown" | "text/x-markdown" | "text/plain" => &mut source_q,
            _ => continue,
        };
        *slot = Some(slot.unwrap_or(0.0).max(quality));
    }
    html_q.is_some_and(|html| html > 0.0 && source_q.is_none_or(|source| html >= source))
}

/// Everything the renderer needs besides the source.
pub struct Page<'a> {
    pub source: &'a str,
    /// Stored path, used for the title when nothing better is available.
    pub path: &'a str,
    /// Where the RAW control points.
    pub raw_href: &'a str,
    /// URL prefix for built-in assets, from [`crate::assets::base`].
    pub assets: &'a str,
}

/// Renders a Markdown file as a complete HTML document.
pub fn render(page: &Page<'_>) -> String {
    let (front_matter, body) = split_front_matter(page.source);
    let mut problems = Vec::new();
    let data = match front_matter {
        None => Value::Object(Map::new()),
        Some((syntax, text)) => match parse_front_matter(syntax, text) {
            Ok(value) => value,
            Err(problem) => {
                problems.push(problem);
                Value::Object(Map::new())
            }
        },
    };
    let settings = Settings::from_front_matter(&data, &mut problems);
    let rendered = render_body(body, &settings);
    let title = settings
        .title
        .clone()
        .or_else(|| rendered.first_h1.clone())
        .unwrap_or_else(|| file_name(page.path).to_string());
    document(page, &settings, &rendered, &title, &data, &problems).into_string()
}

// ---------------------------------------------------------------------------
// Front matter

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Syntax {
    Yaml,
    Toml,
}

/// Splits leading front matter from the body.
///
/// Front matter must open on the very first line (after an optional BOM) and
/// must close, or it is not front matter at all: a lone leading `---` is an
/// ordinary Markdown thematic break.
fn split_front_matter(source: &str) -> (Option<(Syntax, &str)>, &str) {
    let text = source.strip_prefix('\u{feff}').unwrap_or(source);
    let Some((first, rest)) = text.split_once('\n') else {
        return (None, source);
    };
    let syntax = match first.trim_end() {
        "---" => Syntax::Yaml,
        "+++" => Syntax::Toml,
        _ => return (None, source),
    };
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        let bare = line.trim_end();
        let closes = match syntax {
            Syntax::Yaml => bare == "---" || bare == "...",
            Syntax::Toml => bare == "+++",
        };
        if closes {
            return (
                Some((syntax, &rest[..offset])),
                &rest[offset + line.len()..],
            );
        }
        offset += line.len();
    }
    (None, source)
}

fn parse_front_matter(syntax: Syntax, text: &str) -> Result<Value, String> {
    let value = match syntax {
        Syntax::Yaml => {
            use saphyr::LoadableYamlNode as _;
            let documents = saphyr::Yaml::load_from_str(text)
                .map_err(|error| format!("YAML front matter: {error}"))?;
            documents.first().map_or(Value::Null, yaml_to_json)
        }
        Syntax::Toml => {
            let table: toml::Table = text.parse().map_err(|error: toml::de::Error| {
                format!("TOML front matter: {}", error.message())
            })?;
            toml_to_json(toml::Value::Table(table))
        }
    };
    match value {
        Value::Object(_) => Ok(value),
        Value::Null => Ok(Value::Object(Map::new())),
        _ => Err("front matter must be a mapping of keys to values".to_string()),
    }
}

fn yaml_to_json(yaml: &saphyr::Yaml<'_>) -> Value {
    use saphyr::{Scalar, Yaml};
    match yaml {
        Yaml::Value(Scalar::Null) | Yaml::Alias(_) | Yaml::BadValue => Value::Null,
        Yaml::Value(Scalar::Boolean(value)) => Value::Bool(*value),
        Yaml::Value(Scalar::Integer(value)) => Value::from(*value),
        Yaml::Value(Scalar::FloatingPoint(value)) => serde_json::Number::from_f64(value.0)
            .map_or_else(|| Value::String(value.0.to_string()), Value::Number),
        Yaml::Value(Scalar::String(value)) | Yaml::Representation(value, _, _) => {
            Value::String(value.to_string())
        }
        Yaml::Sequence(items) => Value::Array(items.iter().map(yaml_to_json).collect()),
        Yaml::Mapping(entries) => Value::Object(
            entries
                .iter()
                .map(|(key, value)| (yaml_key(key), yaml_to_json(value)))
                .collect(),
        ),
        Yaml::Tagged(_, inner) => yaml_to_json(inner),
    }
}

fn yaml_key(key: &saphyr::Yaml<'_>) -> String {
    match yaml_to_json(key) {
        Value::String(text) => text,
        other => other.to_string(),
    }
}

fn toml_to_json(value: toml::Value) -> Value {
    match value {
        toml::Value::String(text) => Value::String(text),
        toml::Value::Integer(number) => Value::from(number),
        toml::Value::Float(number) => serde_json::Number::from_f64(number)
            .map_or_else(|| Value::String(number.to_string()), Value::Number),
        toml::Value::Boolean(flag) => Value::Bool(flag),
        toml::Value::Datetime(when) => Value::String(when.to_string()),
        toml::Value::Array(items) => Value::Array(items.into_iter().map(toml_to_json).collect()),
        toml::Value::Table(table) => Value::Object(
            table
                .into_iter()
                .map(|(key, value)| (key, toml_to_json(value)))
                .collect(),
        ),
    }
}

/// The front matter keys the renderer understands.
///
/// Unknown keys are not an error: the whole front matter is also published to
/// scripts on the page, so a site is free to invent its own.
#[allow(clippy::struct_excessive_bools)]
struct Settings {
    title: Option<String>,
    description: Option<String>,
    lang: Option<String>,
    theme: &'static str,
    font: &'static str,
    width: &'static str,
    css: Option<String>,
    stylesheets: Vec<String>,
    scripts: Vec<String>,
    head: Option<String>,
    body_class: Option<String>,
    controls: Vec<&'static str>,
    math: bool,
    highlight: bool,
    toc: bool,
    smart_punctuation: bool,
}

impl Settings {
    fn from_front_matter(data: &Value, problems: &mut Vec<String>) -> Self {
        let empty = Map::new();
        let map = data.as_object().unwrap_or(&empty);
        let mut reader = Reader { map, problems };
        Self {
            title: reader.string("title"),
            description: reader.string("description"),
            lang: reader.string("lang"),
            theme: reader.choice("theme", &THEMES, "auto"),
            font: reader.choice("font", &FONTS, "sans"),
            width: reader.choice("width", &WIDTHS, "narrow"),
            css: reader.string("css"),
            stylesheets: reader.strings(&["stylesheets", "stylesheet"]),
            scripts: reader.strings(&["scripts", "script"]),
            head: reader.string("head"),
            body_class: reader.string("class"),
            controls: reader.controls(),
            math: reader.flag("math", true),
            highlight: reader.flag("highlight", true),
            toc: reader.flag("toc", false),
            smart_punctuation: reader.flag("smart_punctuation", false),
        }
    }
}

/// Reads typed values out of front matter, recording anything it rejects.
struct Reader<'a> {
    map: &'a Map<String, Value>,
    problems: &'a mut Vec<String>,
}

impl Reader<'_> {
    fn string(&mut self, key: &str) -> Option<String> {
        match self.map.get(key)? {
            Value::String(text) => Some(text.clone()),
            Value::Null => None,
            Value::Number(number) => Some(number.to_string()),
            Value::Bool(flag) => Some(flag.to_string()),
            _ => {
                self.problems.push(format!("`{key}` must be a string"));
                None
            }
        }
    }

    fn flag(&mut self, key: &str, default: bool) -> bool {
        match self.map.get(key) {
            None | Some(Value::Null) => default,
            Some(Value::Bool(flag)) => *flag,
            Some(_) => {
                self.problems.push(format!("`{key}` must be true or false"));
                default
            }
        }
    }

    fn choice(
        &mut self,
        key: &str,
        allowed: &[&'static str],
        default: &'static str,
    ) -> &'static str {
        let Some(wanted) = self.string(key) else {
            return default;
        };
        let wanted = wanted.trim().to_ascii_lowercase();
        if let Some(found) = allowed.iter().find(|option| **option == wanted) {
            return found;
        }
        self.problems.push(format!(
            "`{key}: {wanted}` is not one of {}",
            allowed.join(", ")
        ));
        default
    }

    fn strings(&mut self, keys: &[&str]) -> Vec<String> {
        let mut out = Vec::new();
        for key in keys {
            match self.map.get(*key) {
                None | Some(Value::Null) => {}
                Some(Value::String(text)) => out.push(text.clone()),
                Some(Value::Array(items)) => {
                    for item in items {
                        match item {
                            Value::String(text) => out.push(text.clone()),
                            _ => self.problems.push(format!("`{key}` entries must be URLs")),
                        }
                    }
                }
                Some(_) => self
                    .problems
                    .push(format!("`{key}` must be a URL or a list of URLs")),
            }
        }
        out
    }

    fn controls(&mut self) -> Vec<&'static str> {
        match self.map.get("controls") {
            None | Some(Value::Null | Value::Bool(true)) => CONTROLS.to_vec(),
            Some(Value::Bool(false)) => Vec::new(),
            Some(Value::String(one)) => self.named_controls(std::slice::from_ref(one)),
            Some(Value::Array(items)) => {
                let names: Vec<String> = items
                    .iter()
                    .filter_map(|item| item.as_str().map(str::to_string))
                    .collect();
                self.named_controls(&names)
            }
            Some(_) => {
                self.problems.push(
                    "`controls` must be true, false, or a list of theme, font, width, size, raw"
                        .to_string(),
                );
                CONTROLS.to_vec()
            }
        }
    }

    fn named_controls(&mut self, names: &[String]) -> Vec<&'static str> {
        let mut chosen = Vec::new();
        for name in names {
            let name = name.trim().to_ascii_lowercase();
            match CONTROLS.iter().find(|control| **control == name) {
                Some(control) if !chosen.contains(control) => chosen.push(*control),
                Some(_) => {}
                None => self.problems.push(format!(
                    "`controls` has unknown `{name}`; expected {}",
                    CONTROLS.join(", ")
                )),
            }
        }
        chosen
    }
}

// ---------------------------------------------------------------------------
// Body

struct Heading {
    level: u8,
    id: String,
    text: String,
}

struct RenderedBody {
    html: String,
    headings: Vec<Heading>,
    first_h1: Option<String>,
}

/// Renders the Markdown body, adding what plain CommonMark leaves out.
///
/// On top of pulldown-cmark's output this gives every heading a stable id and
/// a hover anchor, turns ```` ```math ```` fences into display math, and moves
/// footnote definitions into a section at the end. Everything goes through a
/// single HTML writer so footnote numbering stays consistent between the
/// references and the relocated definitions.
fn render_body(markdown: &str, settings: &Settings) -> RenderedBody {
    let mut options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_GFM
        | Options::ENABLE_DEFINITION_LIST
        | Options::ENABLE_SUPERSCRIPT;
    if settings.math {
        options |= Options::ENABLE_MATH;
    }
    if settings.smart_punctuation {
        options |= Options::ENABLE_SMART_PUNCTUATION;
    }

    let mut rewriter = Rewriter::new(settings.math);
    for event in Parser::new_ext(markdown, options) {
        rewriter.push(event);
    }
    let Rewriter {
        mut events,
        mut footnotes,
        headings,
        first_h1,
        ..
    } = rewriter;
    if !footnotes.is_empty() {
        events.push(Event::Html(CowStr::from(
            r#"<section class="symbol-footnotes" aria-label="Footnotes">"#,
        )));
        events.append(&mut footnotes);
        events.push(Event::Html(CowStr::from("</section>")));
    }

    let mut html = String::with_capacity(markdown.len() * 3 / 2);
    pulldown_cmark::html::push_html(&mut html, events.into_iter());
    RenderedBody {
        html,
        headings,
        first_h1,
    }
}

/// Rewrites pulldown-cmark's event stream on its way to the HTML writer.
struct Rewriter<'a> {
    math: bool,
    events: Vec<Event<'a>>,
    /// Footnote definitions, held back so they can be emitted at the end.
    footnotes: Vec<Event<'a>>,
    in_footnote: bool,
    /// An open heading and its inline events, until its end tag arrives.
    heading: Option<(Tag<'a>, Vec<Event<'a>>)>,
    /// The TeX collected from an open ```` ```math ```` fence.
    math_fence: Option<String>,
    headings: Vec<Heading>,
    first_h1: Option<String>,
    used_ids: HashMap<String, usize>,
}

impl<'a> Rewriter<'a> {
    fn new(math: bool) -> Self {
        Self {
            math,
            events: Vec::new(),
            footnotes: Vec::new(),
            in_footnote: false,
            heading: None,
            math_fence: None,
            headings: Vec::new(),
            first_h1: None,
            used_ids: HashMap::new(),
        }
    }

    /// Where finished events go: the footnote section or the main flow.
    const fn output(&mut self) -> &mut Vec<Event<'a>> {
        if self.in_footnote {
            &mut self.footnotes
        } else {
            &mut self.events
        }
    }

    fn push(&mut self, event: Event<'a>) {
        if let Some(tex) = self.math_fence.as_mut() {
            match event {
                Event::Text(text) => tex.push_str(&text),
                Event::End(TagEnd::CodeBlock) => {
                    let tex = self.math_fence.take().unwrap_or_default();
                    self.output().push(Event::DisplayMath(CowStr::from(tex)));
                }
                _ => {}
            }
            return;
        }
        if let Some((_, inline)) = self.heading.as_mut() {
            match event {
                Event::End(TagEnd::Heading(_)) => self.finish_heading(),
                other => inline.push(other),
            }
            return;
        }
        match event {
            Event::Start(tag @ Tag::Heading { .. }) => self.heading = Some((tag, Vec::new())),
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(ref info)))
                if self.math && info.split_whitespace().next() == Some("math") =>
            {
                self.math_fence = Some(String::new());
            }
            Event::Start(Tag::FootnoteDefinition(_)) => {
                self.in_footnote = true;
                self.footnotes.push(event);
            }
            Event::End(TagEnd::FootnoteDefinition) => {
                self.footnotes.push(event);
                self.in_footnote = false;
            }
            other => self.output().push(other),
        }
    }

    /// Emits a buffered heading with a stable id and a hover anchor.
    fn finish_heading(&mut self) {
        let Some((
            Tag::Heading {
                level,
                id,
                classes,
                attrs,
            },
            inline,
        )) = self.heading.take()
        else {
            return;
        };
        let text = plain_text(&inline);
        let id = id.map_or_else(
            || unique_id(&slug(&text), &mut self.used_ids),
            |id| id.to_string(),
        );
        let rank = heading_rank(level);
        if rank == 1 && self.first_h1.is_none() && !text.is_empty() {
            self.first_h1 = Some(text.clone());
        }
        let anchor = format!(
            r##"<a class="anchor" href="#{}" aria-label="Link to this section">#</a>"##,
            escape_attribute(&id)
        );
        self.headings.push(Heading {
            level: rank,
            id: id.clone(),
            text,
        });
        let output = self.output();
        output.push(Event::Start(Tag::Heading {
            level,
            id: Some(CowStr::from(id)),
            classes,
            attrs,
        }));
        output.extend(inline);
        output.push(Event::Html(CowStr::from(anchor)));
        output.push(Event::End(TagEnd::Heading(level)));
    }
}

const fn heading_rank(level: pulldown_cmark::HeadingLevel) -> u8 {
    use pulldown_cmark::HeadingLevel;
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn plain_text(events: &[Event<'_>]) -> String {
    let mut text = String::new();
    for event in events {
        match event {
            Event::Text(part) | Event::Code(part) | Event::InlineMath(part) => text.push_str(part),
            Event::SoftBreak | Event::HardBreak => text.push(' '),
            _ => {}
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A GitHub-style heading id: lower case, words joined by hyphens.
fn slug(text: &str) -> String {
    let mut slug = String::with_capacity(text.len());
    for character in text.chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() || character == '_' || character == '-' {
            slug.push(character);
        } else if character.is_whitespace() {
            slug.push('-');
        }
    }
    if slug.is_empty() {
        "section".to_string()
    } else {
        slug
    }
}

fn unique_id(base: &str, used: &mut HashMap<String, usize>) -> String {
    let count = used.entry(base.to_string()).or_insert(0);
    let id = if *count == 0 {
        base.to_string()
    } else {
        format!("{base}-{count}")
    };
    *count += 1;
    id
}

// ---------------------------------------------------------------------------
// Document

fn document(
    page: &Page<'_>,
    settings: &Settings,
    body: &RenderedBody,
    title: &str,
    data: &Value,
    problems: &[String],
) -> Markup {
    let assets = page.assets;
    let show = |name: &str| settings.controls.contains(&name);
    html! {
        (DOCTYPE)
        html lang=(settings.lang.as_deref().unwrap_or("en"))
            data-theme=(settings.theme)
            data-font=(settings.font)
            data-width=(settings.width)
        {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                meta name="generator" content="symbol";
                title { (title) }
                @if let Some(description) = &settings.description {
                    meta name="description" content=(description);
                }
                link rel="alternate" type="text/markdown" href=(page.raw_href);
                // Applies the reader's saved choices before first paint, so a
                // dark-mode reader never sees a white flash.
                script { (PreEscaped(EARLY_PREFERENCES)) }
                link rel="stylesheet" href={ (assets) "/markdown.css" };
                @if settings.math {
                    link rel="stylesheet" href={ (assets) "/katex/katex.min.css" };
                }
                @for stylesheet in &settings.stylesheets {
                    link rel="stylesheet" href=(stylesheet);
                }
                @if let Some(css) = &settings.css {
                    style { (PreEscaped(css)) }
                }
                @if let Some(head) = &settings.head {
                    (PreEscaped(head))
                }
            }
            body class=[settings.body_class.as_deref()] {
                @if !settings.controls.is_empty() {
                    (controls(page.raw_href, &show))
                }
                main class="symbol-markdown" {
                    @if !problems.is_empty() {
                        aside class="symbol-front-matter-error" role="note" {
                            strong { "Front matter was not fully applied." }
                            ul {
                                @for problem in problems {
                                    li { (problem) }
                                }
                            }
                        }
                    }
                    @if settings.toc && body.headings.iter().any(|heading| heading.level > 1) {
                        (table_of_contents(&body.headings))
                    }
                    article class="markdown-body" { (PreEscaped(&body.html)) }
                }
                script type="application/json" id="symbol-front-matter" {
                    (PreEscaped(script_safe_json(data)))
                }
                @if settings.math {
                    script src={ (assets) "/katex/katex.min.js" } defer {}
                    script src={ (assets) "/katex/copy-tex.min.js" } defer {}
                }
                @if settings.highlight {
                    script src={ (assets) "/highlight/highlight.min.js" } defer {}
                }
                script src={ (assets) "/markdown.js" } defer {}
                @for script in &settings.scripts {
                    script src=(script) defer {}
                }
            }
        }
    }
}

/// Reads the reader's saved theme, width and scale before first paint.
///
/// Kept in step with `static/markdown.js`, which owns the same keys.
const EARLY_PREFERENCES: &str = r#"(()=>{try{const s=localStorage,r=document.documentElement,o={theme:["auto","light","dark","sepia"],font:["sans","serif","mono"],width:["narrow","wide","full"]};for(const k in o){const v=s.getItem("symbol-md-"+k);if(o[k].includes(v))r.dataset[k]=v}const z=[.85,.92,1,1.08,1.17,1.28][Number(s.getItem("symbol-md-scale"))];if(z)r.style.setProperty("--md-scale",String(z))}catch{}})()"#;

fn controls(raw_href: &str, show: &dyn Fn(&str) -> bool) -> Markup {
    let settings = ["theme", "font", "width", "size"].into_iter().any(show);
    html! {
        nav class="symbol-controls" aria-label="Reading controls" {
            @if show("raw") {
                a class="md-button symbol-raw" href=(raw_href) title="View the exact Markdown source" {
                    "RAW"
                }
            }
            // Hidden until markdown.js wires it up; without script only the
            // RAW link, which needs none, is offered.
            @if settings {
                details class="symbol-settings" hidden {
                    summary class="md-button" title="Reading settings" aria-label="Reading settings" {
                        span class="symbol-glyph" aria-hidden="true" { "Aa" }
                    }
                    div class="symbol-panel" {
                        @if show("theme") { (choice_group("theme", "Theme", &THEMES)) }
                        @if show("font") { (choice_group("font", "Font", &FONTS)) }
                        @if show("width") { (choice_group("width", "Width", &WIDTHS)) }
                        @if show("size") {
                            div class="symbol-choice" role="group" aria-label="Text size" {
                                span class="symbol-choice-label" aria-hidden="true" { "Size" }
                                div class="symbol-choice-options" {
                                    button type="button" class="md-button md-button-plain"
                                        data-symbol-action="smaller" aria-label="Smaller text" { "A\u{2212}" }
                                    button type="button" class="md-button md-button-plain"
                                        data-symbol-action="reset-size" aria-label="Reset text size" { "100%" }
                                    button type="button" class="md-button md-button-plain"
                                        data-symbol-action="larger" aria-label="Larger text" { "A+" }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn choice_group(key: &str, label: &str, values: &[&str]) -> Markup {
    html! {
        div class="symbol-choice" role="group" aria-label=(label) {
            span class="symbol-choice-label" aria-hidden="true" { (label) }
            div class="symbol-choice-options" {
                @for value in values {
                    button type="button" class="md-button md-button-plain"
                        data-symbol-set=(key) value=(value) aria-pressed="false" {
                        (capitalized(value))
                    }
                }
            }
        }
    }
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

fn table_of_contents(headings: &[Heading]) -> Markup {
    // `open` holds the heading level of each `<ul>` still open. A deeper
    // heading nests a list inside the current item; a shallower one closes
    // lists until it finds its own. A heading that skips back past an
    // intermediate level becomes a sibling at the nearest open level, which is
    // the least surprising reading of a malformed outline.
    let mut list = String::new();
    let mut open: Vec<u8> = Vec::new();
    for heading in headings.iter().filter(|heading| heading.level > 1) {
        let level = heading.level;
        match open.last() {
            // The first entry, or one deeper than the current: nest inside the
            // item that is still open.
            None => {
                list.push_str("<ul>");
                open.push(level);
            }
            Some(&top) if level > top => {
                list.push_str("<ul>");
                open.push(level);
            }
            Some(_) => {
                list.push_str("</li>");
                while open.len() > 1 && open.last().is_some_and(|&top| level < top) {
                    list.push_str("</ul>");
                    open.pop();
                    // Back inside the parent item. If this heading is still
                    // deeper than the parent, it belongs under it.
                    if open.last().is_some_and(|&top| level > top) {
                        list.push_str("<ul>");
                        open.push(level);
                        break;
                    }
                    list.push_str("</li>");
                }
            }
        }
        write!(
            list,
            r##"<li><a href="#{}">{}</a>"##,
            escape_attribute(&heading.id),
            escape_text(&heading.text)
        )
        .expect("writing to String cannot fail");
    }
    if !open.is_empty() {
        list.push_str("</li>");
        for _ in 1..open.len() {
            list.push_str("</ul></li>");
        }
        list.push_str("</ul>");
    }
    html! {
        nav class="symbol-toc" aria-label="Contents" {
            strong { "Contents" }
            (PreEscaped(list))
        }
    }
}

/// Serializes JSON so it cannot close the `<script>` element it sits in.
fn script_safe_json(value: &Value) -> String {
    serde_json::to_string(value)
        .expect("JSON values always serialize")
        .replace('<', "\\u003c")
        .replace('>', "\\u003e")
        .replace('&', "\\u0026")
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn escape_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(character),
        }
    }
    out
}

fn escape_attribute(text: &str) -> String {
    escape_text(text)
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn page(source: &str) -> String {
        render(&Page {
            source,
            path: "docs/notes.md",
            raw_href: "notes.md/RAW",
            assets: "/ASSETS/test",
        })
    }

    fn accept(value: &'static str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::ACCEPT, HeaderValue::from_static(value));
        headers
    }

    #[test]
    fn recognises_markdown_extensions() {
        for path in ["notes.md", "a/b/README.MD", "x.markdown", "Doc.Markdown"] {
            assert!(is_markdown(path), "{path}");
        }
        for path in [
            "notes.txt",
            "md",
            "notes.md.bak",
            "folder.md/index.html",
            "markdown",
        ] {
            assert!(!is_markdown(path), "{path}");
        }
    }

    #[test]
    fn only_explicit_html_preference_renders() {
        // A browser navigation.
        assert!(wants_rendering(&accept(
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8"
        )));
        assert!(wants_rendering(&accept("application/xhtml+xml")));
        // `fetch()` and curl.
        assert!(!wants_rendering(&accept("*/*")));
        assert!(!wants_rendering(&HeaderMap::new()));
        // Asking for the source, or preferring it.
        assert!(!wants_rendering(&accept("text/markdown")));
        assert!(!wants_rendering(&accept("text/markdown, text/html;q=0.5")));
        assert!(!wants_rendering(&accept("text/html;q=0")));
        assert!(wants_rendering(&accept("text/html, text/plain;q=0.5")));
        assert!(wants_rendering(&accept("TEXT/HTML;Q=0.9")));
    }

    #[test]
    fn splits_yaml_and_toml_front_matter_only_when_closed() {
        let (matter, body) = split_front_matter("---\ntitle: Hi\n---\n# Body\n");
        assert_eq!(matter, Some((Syntax::Yaml, "title: Hi\n")));
        assert_eq!(body, "# Body\n");

        let (matter, body) = split_front_matter("+++\ntitle = \"Hi\"\n+++\nBody");
        assert_eq!(matter, Some((Syntax::Toml, "title = \"Hi\"\n")));
        assert_eq!(body, "Body");

        let (matter, _) = split_front_matter("---\r\ntitle: Hi\r\n...\r\nBody");
        assert_eq!(matter.map(|(syntax, _)| syntax), Some(Syntax::Yaml));

        let (matter, _) = split_front_matter("\u{feff}---\ntitle: Hi\n---\n");
        assert!(matter.is_some(), "a BOM does not hide front matter");

        // An unclosed `---` is a thematic break, not front matter.
        let source = "---\nnot front matter\n";
        assert_eq!(split_front_matter(source), (None, source));
        let source = "Intro\n---\ntitle: nope\n---\n";
        assert_eq!(split_front_matter(source), (None, source));
    }

    #[test]
    fn front_matter_sets_title_theme_width_and_metadata() {
        let html = page(
            "---\ntitle: \"Caf\u{e9} & <Friends>\"\ndescription: A page\nlang: fr\ntheme: dark\nwidth: wide\n---\n# Heading\n",
        );
        assert!(
            html.contains("<title>Caf\u{e9} &amp; &lt;Friends&gt;</title>"),
            "{html}"
        );
        assert!(html.contains(r#"<meta name="description" content="A page">"#));
        assert!(
            html.contains(
                r#"<html lang="fr" data-theme="dark" data-font="sans" data-width="wide">"#
            )
        );
    }

    #[test]
    fn toml_front_matter_works_the_same() {
        let html = page(
            "+++\ntitle = \"Tom\"\ntheme = \"sepia\"\nfont = \"serif\"\ntoc = true\n+++\n## One\n## Two\n",
        );
        assert!(html.contains("<title>Tom</title>"));
        assert!(html.contains(r#"data-theme="sepia""#));
        assert!(html.contains(r#"data-font="serif""#));
        assert!(html.contains(r#"class="symbol-toc""#));
    }

    #[test]
    fn title_falls_back_to_first_h1_then_file_name() {
        assert!(page("# From *Heading*\n\ntext").contains("<title>From Heading</title>"));
        assert!(page("just text").contains("<title>notes.md</title>"));
    }

    #[test]
    fn bad_front_matter_is_reported_not_swallowed() {
        let html = page("---\ntitle: [unclosed\n---\nBody text\n");
        assert!(html.contains("symbol-front-matter-error"), "{html}");
        assert!(html.contains("Body text"), "the page still renders");

        let html = page("---\ntheme: purple\ncontrols: [theme, sparkles]\n---\nx");
        assert!(html.contains("`theme: purple` is not one of auto, light, dark, sepia"));
        assert!(html.contains("unknown `sparkles`"));
        assert!(
            html.contains(r#"data-theme="auto""#),
            "falls back to the default"
        );
    }

    #[test]
    fn custom_css_stylesheets_scripts_and_head_are_injected() {
        let html = page(concat!(
            "---\n",
            "css: \":root { --md-accent: tomato }\"\n",
            "stylesheets: [theme.css, /shared/site.css]\n",
            "script: extra.js\n",
            "head: '<meta name=\"robots\" content=\"noindex\">'\n",
            "class: essay wide-images\n",
            "---\nx"
        ));
        assert!(html.contains("<style>:root { --md-accent: tomato }</style>"));
        assert!(html.contains(r#"<link rel="stylesheet" href="theme.css">"#));
        assert!(html.contains(r#"<link rel="stylesheet" href="/shared/site.css">"#));
        assert!(html.contains(r#"<script src="extra.js" defer></script>"#));
        assert!(html.contains(r#"<meta name="robots" content="noindex">"#));
        assert!(html.contains(r#"<body class="essay wide-images">"#));
        // Author scripts run after the page's own, so they see rendered math.
        assert!(html.find("/markdown.js").unwrap() < html.find(r#"src="extra.js""#).unwrap());
    }

    #[test]
    fn controls_are_rendered_by_default_and_configurable() {
        let html = page("x");
        for (key, values) in [("theme", &THEMES[..]), ("font", &FONTS), ("width", &WIDTHS)] {
            for value in values {
                assert!(
                    html.contains(&format!(r#"data-symbol-set="{key}" value="{value}""#)),
                    "{key}={value}"
                );
            }
        }
        for action in ["smaller", "reset-size", "larger"] {
            assert!(
                html.contains(&format!(r#"data-symbol-action="{action}""#)),
                "{action}"
            );
        }
        assert!(html.contains(r#"<details class="symbol-settings" hidden>"#));
        assert!(html.contains(r#"<a class="md-button symbol-raw" href="notes.md/RAW""#));

        let none = page("---\ncontrols: false\n---\nx");
        assert!(!none.contains("symbol-controls"));

        let some = page("---\ncontrols: [raw]\n---\nx");
        assert!(some.contains("symbol-raw"));
        assert!(!some.contains("symbol-settings"), "no empty settings panel");

        let font_only = page("---\ncontrols: [font]\n---\nx");
        assert!(font_only.contains(r#"data-symbol-set="font""#));
        assert!(!font_only.contains(r#"data-symbol-set="theme""#));
        assert!(!font_only.contains("symbol-raw"));
    }

    #[test]
    fn raw_html_passes_through() {
        let html = page(
            "<div class=\"hero\"><script>window.x = 1</script></div>\n\nInline <kbd>Ctrl</kbd>.",
        );
        assert!(html.contains(r#"<div class="hero"><script>window.x = 1</script></div>"#));
        assert!(html.contains("<kbd>Ctrl</kbd>"));
    }

    #[test]
    fn math_is_marked_for_katex_and_can_be_disabled() {
        let html = page(
            "Inline $e^{i\\pi} + 1 = 0$ and\n\n$$\\int_0^1 x\\,dx$$\n\n```math\n\\sum_{n} n\n```\n",
        );
        assert!(
            html.contains(r#"<span class="math math-inline">e^{i\pi} + 1 = 0</span>"#),
            "{html}"
        );
        assert!(html.contains(r#"<span class="math math-display">\int_0^1 x\,dx</span>"#));
        assert!(
            html.contains(r#"<span class="math math-display">\sum_{n} n"#),
            "math fences are display math"
        );
        assert!(html.contains("/katex/katex.min.js"));
        assert!(html.contains("/katex/katex.min.css"));

        let off = page("---\nmath: false\n---\nCosts $5 and $10\n");
        assert!(!off.contains("math-inline"));
        assert!(!off.contains("katex"));
    }

    #[test]
    fn prices_are_not_mistaken_for_math() {
        assert!(!page("It costs $5 and then $10 more.").contains("math-inline"));
    }

    #[test]
    fn code_blocks_keep_language_classes_for_highlighting() {
        let html = page("```rust\nfn main() {}\n```\n");
        assert!(html.contains(r#"<code class="language-rust">"#));
        assert!(html.contains("/highlight/highlight.min.js"));
        assert!(!page("---\nhighlight: false\n---\nx").contains("highlight.min.js"));
    }

    #[test]
    fn headings_get_unique_ids_and_anchors() {
        let html = page("## Intro\n## Intro\n## Custom {#mine}\n### Caf\u{e9} & Tea\n");
        assert!(
            html.contains(r##"<h2 id="intro">Intro<a class="anchor" href="#intro""##),
            "{html}"
        );
        assert!(html.contains(r#"<h2 id="intro-1">"#));
        assert!(html.contains(r#"<h2 id="mine">"#));
        assert!(html.contains("<h3 id=\"caf\u{e9}--tea\">"), "{html}");
    }

    #[test]
    fn table_of_contents_nests_by_level() {
        let html = page("---\ntoc: true\n---\n# Title\n## A\n### A1\n## B\n");
        let toc = html.split(r#"class="symbol-toc""#).nth(1).unwrap();
        let toc = toc.split("</nav>").next().unwrap();
        assert!(toc.contains(r##"<a href="#a">A</a><ul><li><a href="#a1">A1</a></li></ul></li><li><a href="#b">B</a>"##), "{toc}");
        assert!(
            !toc.contains("#title"),
            "the page title is not a contents entry"
        );
    }

    #[test]
    fn table_of_contents_handles_skipped_and_shallower_levels() {
        let toc = |source: &str| {
            let html = page(&format!("---\ntoc: true\n---\n{source}"));
            html.split(r#"class="symbol-toc""#)
                .nth(1)
                .unwrap()
                .split("</nav>")
                .next()
                .unwrap()
                .split("<strong>Contents</strong>")
                .nth(1)
                .unwrap()
                .to_string()
        };
        // A deeper heading after a skipped level still nests under its parent.
        assert_eq!(
            toc("## A\n#### B\n### C\n"),
            r##"<ul><li><a href="#a">A</a><ul><li><a href="#b">B</a></li></ul><ul><li><a href="#c">C</a></li></ul></li></ul>"##
        );
        // An outline that starts deep and comes back up stays flat, not broken.
        assert_eq!(
            toc("### X\n## Y\n"),
            r##"<ul><li><a href="#x">X</a></li><li><a href="#y">Y</a></li></ul>"##
        );
        // Balanced tags in every case.
        for source in [
            "## A\n#### B\n### C\n## D\n",
            "#### a\n## b\n###### c\n### d\n",
        ] {
            let list = toc(source);
            assert_eq!(
                list.matches("<ul>").count(),
                list.matches("</ul>").count(),
                "{list}"
            );
            assert_eq!(
                list.matches("<li>").count(),
                list.matches("</li>").count(),
                "{list}"
            );
        }
    }

    #[test]
    fn footnotes_move_to_the_end() {
        let html = page("Claim.[^source]\n\n[^source]: The source.\n\nMore text.\n");
        let body = html.split(r#"class="markdown-body""#).nth(1).unwrap();
        let notes = body.find("symbol-footnotes").expect("footnote section");
        assert!(
            body.find("More text").unwrap() < notes,
            "definitions follow the prose"
        );
        assert!(body[notes..].contains("The source."));
        assert!(body.contains(r#"class="footnote-reference""#));
    }

    #[test]
    fn gfm_features_render() {
        let html = page(concat!(
            "| a | b |\n|---|---|\n| 1 | 2 |\n\n",
            "- [x] done\n- [ ] todo\n\n",
            "~~gone~~ and ^up^\n\n",
            "> [!WARNING]\n> Careful.\n\n",
            "Term\n: Definition\n"
        ));
        assert!(html.contains("<table>"));
        assert!(html.contains(r#"type="checkbox""#));
        assert!(html.contains("<del>gone</del>"));
        assert!(html.contains("<sup>up</sup>"));
        assert!(html.contains("markdown-alert-warning"), "{html}");
        assert!(html.contains("<dt>Term</dt>"));
    }

    #[test]
    fn front_matter_is_published_to_scripts_without_breaking_out() {
        let html = page("---\ncustom: \"</script><script>alert(1)</script>\"\ncount: 3\n---\nx");
        let data = html
            .split(r#"<script type="application/json" id="symbol-front-matter">"#)
            .nth(1)
            .unwrap()
            .split("</script>")
            .next()
            .unwrap();
        assert!(!data.contains('<'), "{data}");
        let parsed: Value = serde_json::from_str(data).unwrap();
        assert_eq!(parsed["custom"], "</script><script>alert(1)</script>");
        assert_eq!(parsed["count"], 3);
    }

    #[test]
    fn assets_resolve_under_the_given_bundle() {
        let html = page("x");
        assert!(html.contains(r#"href="/ASSETS/test/markdown.css""#));
        assert!(html.contains(r#"src="/ASSETS/test/markdown.js""#));
        assert!(
            html.contains(r#"<link rel="alternate" type="text/markdown" href="notes.md/RAW">"#)
        );
    }
}
