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
const WIDTHS: [&str; 4] = ["narrow", "medium", "wide", "full"];
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

/// Where the Markdown guide is served; pages link to it for help.
pub const GUIDE: &str = "/API/MARKDOWN";

/// The URL of a file's exact bytes: its path with `/RAW` appended.
pub fn raw_href(name: &str, path: &str) -> String {
    let mut href = format!("/{name}");
    for segment in path.split('/') {
        href.push('/');
        crate::mutation_http::encode_path_segment(&mut href, segment);
    }
    href.push_str("/RAW");
    href
}

/// Renders a Markdown file as a complete HTML document.
pub fn render(page: &Page<'_>) -> String {
    let (front_matter, body) = split_front_matter(page.source);
    let mut problems = Vec::new();
    let data = match front_matter {
        None => {
            problems.extend(unclosed_front_matter(page.source));
            Value::Object(Map::new())
        }
        Some((syntax, text)) => {
            if syntax == Syntax::Yaml {
                problems.extend(duplicate_yaml_keys(text));
            }
            match parse_front_matter(syntax, text) {
                Ok(value) => value,
                Err(problem) => {
                    problems.push(problem);
                    Value::Object(Map::new())
                }
            }
        }
    };
    let settings = Settings::from_front_matter(&data, &mut problems);
    let rendered = render_body(body, &settings);
    problems.extend(rendered.problems.iter().cloned());
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

/// Explains front matter that was opened but never closed.
///
/// An unclosed `+++` means nothing in Markdown, so it is always a mistake. A
/// lone `---` is a legitimate thematic break, so it is only reported when the
/// next line reads like a front matter key.
fn unclosed_front_matter(source: &str) -> Option<String> {
    let text = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut lines = text.lines();
    match lines.next()?.trim_end() {
        "+++" => Some(
            "Front matter opened with `+++` on line 1 is never closed with `+++`, \
             so it was not applied."
                .to_string(),
        ),
        "---"
            if lines
                .find(|line| !line.trim().is_empty())
                .is_some_and(looks_like_key) =>
        {
            Some(
                "Front matter opened with `---` on line 1 is never closed with `---`, \
                 so it was not applied and shows as text."
                    .to_string(),
            )
        }
        _ => None,
    }
}

fn looks_like_key(line: &str) -> bool {
    let Some((key, _)) = line.split_once(':') else {
        return false;
    };
    let key = key.trim_end();
    !key.is_empty()
        && !line.starts_with(char::is_whitespace)
        && key
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '_' | '-'))
}

/// Finds top-level YAML keys given more than once.
///
/// YAML loaders keep the last value without a word, which would silently
/// discard the first; TOML already rejects duplicates as a parse error.
fn duplicate_yaml_keys(text: &str) -> Vec<String> {
    use saphyr_parser::{Event, Parser};
    let mut problems = Vec::new();
    let mut seen = HashMap::new();
    let mut depth = 0usize;
    let mut expect_key = true;
    for item in Parser::new_from_str(text) {
        let Ok((event, span)) = item else {
            break;
        };
        match event {
            Event::MappingStart(..) | Event::SequenceStart(..) => depth += 1,
            Event::MappingEnd | Event::SequenceEnd => {
                depth = depth.saturating_sub(1);
                if depth == 1 {
                    expect_key = !expect_key;
                }
            }
            Event::Scalar(value, ..) if depth == 1 => {
                if expect_key {
                    let line = span.start.line() + 1;
                    if let Some(first) = seen.insert(value.to_string(), line) {
                        problems.push(format!(
                            "Front matter: `{value}` is set on line {first} and again on \
                             line {line}; only the last value was applied."
                        ));
                    }
                }
                expect_key = !expect_key;
            }
            Event::Alias(..) if depth == 1 => expect_key = !expect_key,
            _ => {}
        }
    }
    problems
}

fn parse_front_matter(syntax: Syntax, text: &str) -> Result<Value, String> {
    let value = match syntax {
        Syntax::Yaml => {
            use saphyr::LoadableYamlNode as _;
            let documents = saphyr::Yaml::load_from_str(text).map_err(|error| {
                // saphyr counts lines from 1 within the block; the opening
                // `---` is line 1 of the file.
                format!(
                    "Front matter: YAML error on line {}: {}. None of it was applied.",
                    error.marker().line() + 1,
                    error.info()
                )
            })?;
            documents.first().map_or(Value::Null, yaml_to_json)
        }
        Syntax::Toml => {
            let table: toml::Table = text.parse().map_err(|error: toml::de::Error| {
                let line = error.span().map_or(2, |span| {
                    text[..span.start.min(text.len())].matches('\n').count() + 2
                });
                format!(
                    "Front matter: TOML error on line {line}: {}. None of it was applied.",
                    error.message()
                )
            })?;
            toml_to_json(toml::Value::Table(table))
        }
    };
    match value {
        Value::Object(_) => Ok(value),
        Value::Null => Ok(Value::Object(Map::new())),
        _ => Err(
            "Front matter must be a mapping of keys to values, such as `title: Notes`. \
             None of it was applied."
                .to_string(),
        ),
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

/// Every top-level front matter key the renderer accepts.
///
/// Anything else is reported, since a misspelt option would otherwise do
/// nothing without a word. A page's own values belong under `data`, which is
/// never interpreted; the whole front matter is still published to scripts.
const KNOWN_KEYS: [&str; 19] = [
    "title",
    "description",
    "lang",
    "theme",
    "font",
    "width",
    "css",
    "stylesheet",
    "stylesheets",
    "script",
    "scripts",
    "head",
    "class",
    "controls",
    "toc",
    "math",
    "highlight",
    "smart_punctuation",
    "data",
];

/// The front matter keys the renderer understands.
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
        for key in map.keys() {
            if !KNOWN_KEYS.contains(&key.as_str()) {
                problems.push(unknown_key(key));
            }
        }
        let mut reader = Reader { map, problems };
        Self {
            title: reader.string("title"),
            description: reader.string("description"),
            lang: reader.string("lang"),
            theme: reader.choice("theme", &THEMES, "auto"),
            font: reader.choice("font", &FONTS, "sans"),
            width: reader.choice("width", &WIDTHS, "medium"),
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
    fn report(&mut self, problem: &str) {
        self.problems.push(format!("Front matter: {problem}"));
    }

    fn string(&mut self, key: &str) -> Option<String> {
        match self.map.get(key)? {
            Value::String(text) => Some(text.clone()),
            Value::Null => None,
            Value::Number(number) => Some(number.to_string()),
            Value::Bool(flag) => Some(flag.to_string()),
            _ => {
                self.report(&format!("`{key}` must be text, not a list or mapping."));
                None
            }
        }
    }

    fn flag(&mut self, key: &str, default: bool) -> bool {
        match self.map.get(key) {
            None | Some(Value::Null) => default,
            Some(Value::Bool(flag)) => *flag,
            Some(_) => {
                self.report(&format!(
                    "`{key}` must be `true` or `false`; using `{default}`."
                ));
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
        self.report(&format!(
            "`{key}: {wanted}` is not one of {}; using `{default}`.",
            code_list(allowed)
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
                            _ => {
                                self.report(&format!("`{key}` entries must be URLs; skipped one."));
                            }
                        }
                    }
                }
                Some(_) => self.report(&format!("`{key}` must be a URL or a list of URLs.")),
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
                let mut names = Vec::new();
                for item in items {
                    match item.as_str() {
                        Some(name) => names.push(name.to_string()),
                        None => self.report(&format!(
                            "`controls` entries must be names, not `{item}`; skipped it."
                        )),
                    }
                }
                self.named_controls(&names)
            }
            Some(_) => {
                self.report(&format!(
                    "`controls` must be `true`, `false`, or a list of {}; showing them all.",
                    code_list(&CONTROLS)
                ));
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
                None => self.report(&format!(
                    "`controls` has unknown `{name}`; expected {}.",
                    code_list(&CONTROLS)
                )),
            }
        }
        chosen
    }
}

/// Formats values as a readable list of code spans: `a`, `b` or `c`.
fn code_list(values: &[&str]) -> String {
    let quoted: Vec<String> = values.iter().map(|value| format!("`{value}`")).collect();
    match quoted.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} or {last}", rest.join(", ")),
        _ => quoted.concat(),
    }
}

fn unknown_key(key: &str) -> String {
    let normal = key.trim().to_ascii_lowercase().replace('-', "_");
    let suggestion = KNOWN_KEYS
        .iter()
        .map(|known| (edit_distance(&normal, known), *known))
        .filter(|(distance, known)| *distance <= 2 && *distance < known.len())
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, known)| known);
    suggestion.map_or_else(
        || {
            format!(
                "Front matter: unknown option `{key}`, ignored. Your own values go under `data`."
            )
        },
        |known| {
            format!(
                "Front matter: unknown option `{key}`; did you mean `{known}`? \
                 Your own values go under `data`."
            )
        },
    )
}

/// Levenshtein distance, for suggesting the option a typo meant.
fn edit_distance(left: &str, right: &str) -> usize {
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    for (row, left_char) in left.chars().enumerate() {
        let mut current = vec![row + 1; right.len() + 1];
        for (column, right_char) in right.iter().enumerate() {
            let substitution = previous[column] + usize::from(left_char != *right_char);
            current[column + 1] = substitution
                .min(previous[column + 1] + 1)
                .min(current[column] + 1);
        }
        previous = current;
    }
    previous[right.len()]
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
    problems: Vec<String>,
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
        | Options::ENABLE_SUPERSCRIPT
        | Options::ENABLE_SUBSCRIPT;
    if settings.math {
        options |= Options::ENABLE_MATH;
    }
    if settings.smart_punctuation {
        options |= Options::ENABLE_SMART_PUNCTUATION;
    }

    let mut rewriter = Rewriter::new(settings.math);
    let parser = Parser::new_ext(markdown, options).into_offset_iter();
    for event in intraword_scripts(markdown, parser) {
        rewriter.push(event);
    }
    rewriter.flush_text();
    let problems = rewriter.problems();
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
        problems,
    }
}

/// Adds Pandoc-style `^sup^` and `~sub~` inside words: `H~2~O`, `2^10^`.
///
/// pulldown-cmark only honours these delimiters at word boundaries, so the
/// intraword forms reach this point as literal text. Adjacent text events
/// are joined and scanned; a pair of delimiters around a run with no spaces
/// becomes the element. Only delimiters written literally count: not ones from
/// character references, nor ones escaped as `\^` or `\~`.
fn intraword_scripts<'a>(
    source: &str,
    parser: impl Iterator<Item = (Event<'a>, std::ops::Range<usize>)>,
) -> Vec<Event<'a>> {
    let mut out = Vec::new();
    // Each text event, with where it starts in the source when it is a
    // verbatim copy of it (so offsets inside it map straight back).
    let mut run: Vec<(CowStr<'a>, Option<usize>)> = Vec::new();
    let mut in_code_block = false;
    // Open images and links, and whether each keeps its text literal: image
    // alt text is plain text, and an autolink's text is its URL.
    let mut literal: Vec<bool> = Vec::new();
    for (event, range) in parser {
        match event {
            Event::Text(text) if !in_code_block && !literal.contains(&true) => {
                let start = (source.get(range.clone()) == Some(&*text)).then_some(range.start);
                run.push((text, start));
            }
            other => {
                flush_scripts(source, &mut run, &mut out);
                match &other {
                    Event::Start(Tag::CodeBlock(_)) => in_code_block = true,
                    Event::End(TagEnd::CodeBlock) => in_code_block = false,
                    Event::Start(Tag::Image { .. }) => literal.push(true),
                    Event::Start(Tag::Link { link_type, .. }) => literal.push(matches!(
                        link_type,
                        pulldown_cmark::LinkType::Autolink | pulldown_cmark::LinkType::Email
                    )),
                    Event::End(TagEnd::Image | TagEnd::Link) => {
                        literal.pop();
                    }
                    _ => {}
                }
                out.push(other);
            }
        }
    }
    flush_scripts(source, &mut run, &mut out);
    out
}

fn flush_scripts<'a>(
    source: &str,
    run: &mut Vec<(CowStr<'a>, Option<usize>)>,
    out: &mut Vec<Event<'a>>,
) {
    let has_delimiter = run
        .iter()
        .any(|(text, start)| start.is_some() && text.contains(['^', '~']));
    if !has_delimiter {
        out.extend(run.drain(..).map(|(text, _)| Event::Text(text)));
        return;
    }
    let mut text = String::new();
    // Byte offsets of delimiters that were written literally in the source.
    let mut live = std::collections::HashSet::new();
    for (part, start) in run.drain(..) {
        if let Some(start) = start {
            for (index, _) in part.match_indices(['^', '~']) {
                let escapes = source.as_bytes()[..start + index]
                    .iter()
                    .rev()
                    .take_while(|byte| **byte == b'\\')
                    .count();
                if escapes % 2 == 0 {
                    live.insert(text.len() + index);
                }
            }
        }
        text.push_str(&part);
    }
    let bytes = text.as_bytes();
    let mut plain_from = 0;
    let mut index = 0;
    while index < bytes.len() {
        let delimiter = bytes[index];
        let opens = live.contains(&index)
            // `~~` is strikethrough, never a subscript delimiter.
            && !(delimiter == b'~' && (bytes.get(index + 1) == Some(&b'~')
                || index > 0 && bytes[index - 1] == b'~'));
        let close = opens.then(|| {
            text[index + 1..]
                .find(|character: char| {
                    character.is_whitespace() || matches!(character, '^' | '~' | '[' | ']')
                })
                .map(|offset| index + 1 + offset)
        });
        if let Some(Some(end)) = close
            && end > index + 1
            && bytes[end] == delimiter
            && live.contains(&end)
            && !(delimiter == b'~' && bytes.get(end + 1) == Some(&b'~'))
        {
            if plain_from < index {
                out.push(Event::Text(CowStr::from(
                    text[plain_from..index].to_string(),
                )));
            }
            let tag = if delimiter == b'^' { "sup" } else { "sub" };
            out.push(Event::InlineHtml(CowStr::from(format!("<{tag}>"))));
            out.push(Event::Text(CowStr::from(text[index + 1..end].to_string())));
            out.push(Event::InlineHtml(CowStr::from(format!("</{tag}>"))));
            index = end + 1;
            plain_from = index;
            continue;
        }
        index += 1;
    }
    if plain_from < text.len() {
        out.push(Event::Text(CowStr::from(text[plain_from..].to_string())));
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
    /// Every heading id emitted, to catch two headings given the same `{#id}`.
    heading_ids: std::collections::HashSet<String>,
    duplicate_ids: Vec<String>,
    in_code_block: bool,
    /// Consecutive prose text, scanned for footnote references that did not
    /// resolve: pulldown-cmark leaves those as literal `[^label]` text.
    pending_text: String,
    unresolved_footnotes: Vec<String>,
    footnote_references: std::collections::HashSet<String>,
    footnote_definitions: Vec<String>,
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
            heading_ids: std::collections::HashSet::new(),
            duplicate_ids: Vec::new(),
            in_code_block: false,
            pending_text: String::new(),
            unresolved_footnotes: Vec::new(),
            footnote_references: std::collections::HashSet::new(),
            footnote_definitions: Vec::new(),
        }
    }

    fn flush_text(&mut self) {
        let text = std::mem::take(&mut self.pending_text);
        let mut rest = text.as_str();
        while let Some(start) = rest.find("[^") {
            rest = &rest[start + 2..];
            let Some(end) = rest.find(']') else {
                break;
            };
            let label = &rest[..end];
            if !label.is_empty()
                && !label.contains(char::is_whitespace)
                && !self.unresolved_footnotes.iter().any(|seen| seen == label)
            {
                self.unresolved_footnotes.push(label.to_string());
            }
            rest = &rest[end + 1..];
        }
    }

    /// Problems found in the body, in reading order.
    fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let defined: Vec<String> = self
            .footnote_definitions
            .iter()
            .map(|label| label.to_lowercase())
            .collect();
        for label in &self.unresolved_footnotes {
            if !defined.contains(&label.to_lowercase()) {
                problems.push(format!(
                    "Footnote `[^{label}]` is referenced but never defined, so it shows as text."
                ));
            }
        }
        let mut seen = Vec::new();
        for label in &self.footnote_definitions {
            let normal = label.to_lowercase();
            if seen.contains(&normal) {
                problems.push(format!(
                    "Footnote `[^{label}]` is defined more than once; only the first is used."
                ));
                continue;
            }
            if !self.footnote_references.contains(&normal) {
                problems.push(format!(
                    "Footnote `[^{label}]` is defined but never referenced."
                ));
            }
            seen.push(normal);
        }
        for id in &self.duplicate_ids {
            problems.push(format!(
                "Several headings have the id `{id}`; links to `#{id}` reach only the first."
            ));
        }
        problems
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
        match &event {
            Event::Text(text) if !self.in_code_block => {
                self.pending_text.push_str(text);
            }
            _ => self.flush_text(),
        }
        match event {
            Event::Start(tag @ Tag::Heading { .. }) => self.heading = Some((tag, Vec::new())),
            Event::End(TagEnd::CodeBlock) => {
                self.in_code_block = false;
                self.output().push(event);
            }
            Event::FootnoteReference(ref label) => {
                self.footnote_references.insert(label.to_lowercase());
                self.output().push(event);
            }
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(ref info)))
                if self.math && info.split_whitespace().next() == Some("math") =>
            {
                self.math_fence = Some(String::new());
            }
            Event::Start(Tag::CodeBlock(_)) => {
                self.in_code_block = true;
                self.output().push(event);
            }
            Event::Start(Tag::FootnoteDefinition(ref label)) => {
                self.footnote_definitions.push(label.to_string());
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
            || {
                // Skip past ids an explicit `{#id}` already took.
                let base = slug(&text);
                loop {
                    let candidate = unique_id(&base, &mut self.used_ids);
                    if !self.heading_ids.contains(&candidate) {
                        break candidate;
                    }
                }
            },
            |id| id.to_string(),
        );
        if !self.heading_ids.insert(id.clone()) && !self.duplicate_ids.contains(&id) {
            self.duplicate_ids.push(id.clone());
        }
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
                script { (PreEscaped(EARLY_ERRORS)) ";" (PreEscaped(EARLY_PREFERENCES)) }
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
                    // Always present so markdown.js can add what only a browser can
                    // see: failed loads, math errors, broken in-page links.
                    aside class="symbol-problems" role="note" aria-label="Problems with this page"
                        hidden[problems.is_empty()]
                    {
                        strong { "Problems with this page" }
                        ul {
                            @for problem in problems {
                                li { (problem_markup(problem)) }
                            }
                        }
                        p class="symbol-problems-help" {
                            "The " a href={ (GUIDE) "#when-something-is-wrong" } { "Markdown guide" }
                            " explains each check."
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

/// Records resource load failures and uncaught script errors from the start.
///
/// Runs before any stylesheet or script is requested, so nothing is missed;
/// `static/markdown.js` lists what it recorded in the page's problem notice.
const EARLY_ERRORS: &str = r#"(()=>{const f=document.symbolErrors=[];addEventListener("error",e=>{const t=e.target;if(t instanceof Element){const u=t.currentSrc||t.src||t.href;if(u)f.push({kind:t.localName,url:String(u)})}else if(e instanceof ErrorEvent)f.push({kind:"error",message:e.message,where:e.filename?e.filename+":"+e.lineno:""});else return;document.dispatchEvent(new Event("symbol:error"))},true)})()"#;

/// Renders a problem, with text between backticks as code.
fn problem_markup(problem: &str) -> Markup {
    html! {
        @for (index, part) in problem.split('`').enumerate() {
            @if index % 2 == 1 { code { (part) } } @else { (part) }
        }
    }
}

/// Reads the reader's saved theme, width and scale before first paint.
///
/// Kept in step with `static/markdown.js`, which owns the same keys.
const EARLY_PREFERENCES: &str = r#"(()=>{try{const s=localStorage,r=document.documentElement,o={theme:["auto","light","dark","sepia"],font:["sans","serif","mono"],width:["narrow","medium","wide","full"]};for(const k in o){const v=s.getItem("symbol-md-"+k);if(o[k].includes(v))r.dataset[k]=v}const z=[.85,.92,1,1.08,1.17,1.28][s.getItem("symbol-md-scale")];if(z)r.style.setProperty("--md-scale",String(z))}catch{}})()"#;

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
                        a class="symbol-guide-link" href=(GUIDE) { "How to write and style Markdown pages" }
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

    fn problems(html: &str) -> Vec<String> {
        let Some(start) = html.find(r#"<aside class="symbol-problems""#) else {
            return Vec::new();
        };
        let notice = &html[start..html[start..].find("</aside>").unwrap() + start];
        notice
            .split("<li>")
            .skip(1)
            .map(|item| {
                item.split("</li>")
                    .next()
                    .unwrap()
                    .replace("<code>", "`")
                    .replace("</code>", "`")
            })
            .collect()
    }

    #[test]
    fn a_clean_page_has_an_empty_hidden_notice() {
        let html =
            page("---\ntitle: Fine\ndata: {tags: [a, b], anything: {goes: here}}\n---\n# Hi\n");
        assert!(
            html.contains(r#"aria-label="Problems with this page" hidden>"#),
            "{html}"
        );
        assert!(problems(&html).is_empty(), "{:?}", problems(&html));
    }

    #[test]
    fn unknown_front_matter_options_are_reported_with_suggestions() {
        let found = problems(&page(
            "---\ntittle: Hi\nTheme: dark\nsmart-punctuation: true\nauthor: Sam\n---\nx",
        ));
        assert!(
            found
                .iter()
                .any(|p| p.contains("unknown option `tittle`; did you mean `title`?")),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .any(|p| p.contains("`Theme`; did you mean `theme`?")),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .any(|p| p.contains("`smart-punctuation`; did you mean `smart_punctuation`?")),
            "{found:?}"
        );
        assert!(
            found.iter().any(|p| p
                .contains("unknown option `author`, ignored. Your own values go under `data`.")),
            "{found:?}"
        );
        assert_eq!(found.len(), 4, "{found:?}");
    }

    #[test]
    fn duplicate_yaml_keys_are_reported_with_lines() {
        let found = problems(&page(
            "---\ntitle: One\nnested: {title: fine}\ntitle: Two\n---\nx",
        ));
        assert!(
            found
                .iter()
                .any(|p| p.contains("`title` is set on line 2 and again on line 4")),
            "{found:?}"
        );
        assert!(
            !found.iter().any(|p| p.contains("line 3")),
            "nested keys are separate: {found:?}"
        );
    }

    #[test]
    fn parse_errors_point_at_file_lines() {
        let found = problems(&page("+++\ntitle = \"a\"\ntheme =\n+++\nx"));
        assert!(
            found
                .iter()
                .any(|p| p.starts_with("Front matter: TOML error on line 3")),
            "{found:?}"
        );
        let found = problems(&page("+++\ntitle = \"a\"\ntitle = \"b\"\n+++\nx"));
        assert!(
            found.iter().any(|p| p.contains("duplicate key")),
            "{found:?}"
        );
        let found = problems(&page("---\n- a\n- b\n---\nx"));
        assert!(
            found.iter().any(|p| p.contains("must be a mapping")),
            "{found:?}"
        );
    }

    #[test]
    fn unclosed_front_matter_is_reported_but_a_thematic_break_is_not() {
        let found = problems(&page("+++\ntitle = \"a\"\n\n# Body\n"));
        assert!(
            found
                .iter()
                .any(|p| p.contains("`+++` on line 1 is never closed")),
            "{found:?}"
        );
        let found = problems(&page("---\ntitle: a\n\n# Body\n"));
        assert!(
            found
                .iter()
                .any(|p| p.contains("`---` on line 1 is never closed")),
            "{found:?}"
        );
        assert!(problems(&page("---\n\nJust a rule, then prose: with a colon.\n")).is_empty());
    }

    #[test]
    fn footnote_mistakes_are_reported() {
        let found = problems(&page(concat!(
            "Used[^a], missing[^nope], again[^nope].\n\n",
            "```\ncode [^in-code] is fine\n```\n\n",
            "[^a]: One.\n\n[^unused]: Two.\n\n[^a]: Again.\n"
        )));
        assert_eq!(
            found,
            [
                "Footnote `[^nope]` is referenced but never defined, so it shows as text.",
                "Footnote `[^unused]` is defined but never referenced.",
                "Footnote `[^a]` is defined more than once; only the first is used.",
            ],
        );
    }

    #[test]
    fn duplicate_heading_ids_are_reported_and_generated_ids_avoid_them() {
        let html = page("## Intro {#same}\n\n## Other {#same}\n\n# setup {#setup}\n\n## Setup\n");
        let found = problems(&html);
        assert_eq!(
            found,
            ["Several headings have the id `same`; links to `#same` reach only the first."]
        );
        assert!(
            html.contains(r#"id="setup-1""#),
            "generated id steps past the explicit one: {html}"
        );
    }

    #[test]
    fn superscript_and_subscript_work_inside_words() {
        let body = |source: &str| {
            let html = page(source);
            let start = html.find(r#"<article class="markdown-body">"#).unwrap();
            html[start..html.find("</article>").unwrap()].to_string()
        };
        assert!(
            body("H~2~O and ^super^script").contains("H<sub>2</sub>O and <sup>super</sup>script")
        );
        assert!(
            body("x^2^ and 2^10^, E = mc^2^.")
                .contains("x<sup>2</sup> and 2<sup>10</sup>, E = mc<sup>2</sup>.")
        );
        assert!(body("a ^up^ b ~down~ c").contains("a <sup>up</sup> b <sub>down</sub> c"));
        assert!(body("~~strike~~ and ~sub~").contains("<del>strike</del> and <sub>sub</sub>"));
        // Spaces, escapes, code and footnote syntax stay literal.
        for literal in [
            "~/path/file and ~user",
            "cost ~5 and ~10",
            r"2\^10\^ and H\~2\~O",
            "`x^2^` in code",
            "[^a][^b] undefined",
            "a~~b~~c",
            "<https://example.com/~alice/~bob>",
        ] {
            let html = body(literal);
            assert!(
                !html.contains("<sup>") && !html.contains("<sub>"),
                "{literal}: {html}"
            );
        }
        assert!(
            !body("```\nH~2~O\n```\n").contains("<sub>"),
            "code blocks are verbatim"
        );
        assert!(body("![H~2~O](water.png)").contains(r#"alt="H~2~O""#));
        assert!(
            body("[H~2~O](water.html)").contains("H<sub>2</sub>O</a>"),
            "ordinary link text works"
        );
        // Headings keep the digits in their text, so ids and the TOC read right.
        let html = page("---\ntoc: true\n---\n## Using H~2~O\n");
        assert!(html.contains(r#"id="using-h2o""#), "{html}");
    }

    #[test]
    fn bad_front_matter_is_reported_not_swallowed() {
        let html = page("---\ntitle: [unclosed\n---\nBody text\n");
        assert!(html.contains("YAML error on line 3"), "{html}");
        assert!(html.contains("Body text"), "the page still renders");

        let html = page("---\ntheme: purple\ncontrols: [theme, sparkles, 7]\n---\nx");
        assert!(
            html.contains(
                "<code>theme: purple</code> is not one of <code>auto</code>, <code>light</code>, \
             <code>dark</code> or <code>sepia</code>; using <code>auto</code>."
            ),
            "{html}"
        );
        assert!(html.contains("unknown <code>sparkles</code>"));
        assert!(html.contains("entries must be names, not <code>7</code>"));
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
        assert!(
            html.contains(r#"data-width="medium""#),
            "medium is the default"
        );
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
