//! What of a file goes into the content index: a title and the visible text.
//!
//! Every file whose bytes read as text is indexed, whatever its extension
//! (decision 6 of the search design). HTML keeps its visible text, Markdown
//! its prose and code, and anything else is taken whole, so code, JSON, SVG
//! and CSV are searchable too. Binary files, empty files and files over
//! [`MAX_BLOB_BYTES`] are skipped, and the skip is remembered so they are
//! never read again.

use ego_tree::NodeRef;
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use scraper::{Html, Node};

use crate::markdown;
use crate::sanitize;

/// Files larger than this are skipped unread: parsing bounds memory by
/// holding the whole document.
pub const MAX_BLOB_BYTES: u64 = 16 * 1024 * 1024;
/// Only this much extracted text is indexed, so one huge file cannot swamp
/// the index.
pub const MAX_TEXT_BYTES: usize = 1024 * 1024;
/// A title longer than this is cut; it is a label, not the text.
const MAX_TITLE_CHARS: usize = 200;

/// Marks the start of a highlighted hit in a snippet. It and [`HIT_END`] are
/// private-use characters that the extractor strips from every field, so a
/// snippet is unambiguous without escaping.
pub const HIT_START: char = '\u{E000}';
pub const HIT_END: char = '\u{E001}';

/// What the index stores for one file.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Document {
    /// Empty when the file has none; a renderer falls back to the name.
    pub title: String,
    pub body: String,
}

/// Elements whose text is never visible, or is site chrome rather than
/// content. `nav` and `data-pagefind-ignore` follow Pagefind's conventions,
/// which generated sites already use to mark their menus and footers.
const HIDDEN_ELEMENTS: [&str; 7] = [
    "script", "style", "template", "noscript", "svg", "math", "nav",
];

/// The title and text of the file at `path`, or `None` for a file that is
/// not text and so is skipped.
pub fn extract(path: &str, bytes: &[u8]) -> Option<Document> {
    if bytes.is_empty() || !sanitize::is_text_like(bytes) {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let name = path.rsplit('/').next().unwrap_or(path);
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase());
    let (title, body) = match extension.as_deref() {
        Some("html" | "htm" | "xhtml") => html(text),
        _ if markdown::is_markdown(path) => markdown(text),
        _ => plain(text),
    };
    let mut document = Document {
        title: clean(&title),
        body: clean(&body),
    };
    truncate_chars(&mut document.title, MAX_TITLE_CHARS);
    truncate_bytes(&mut document.body, MAX_TEXT_BYTES);
    Some(document)
}

/// `<title>`, else the first `<h1>`; and the visible text, which is only the
/// `data-pagefind-body` elements when a page marks them.
fn html(source: &str) -> (String, String) {
    let html = Html::parse_document(source);
    let root = html.tree.root();
    let element_named = |name: &str| {
        root.descendants()
            .find(|node| element_name(node) == Some(name))
    };
    let title = element_named("title")
        .or_else(|| element_named("h1"))
        .map(|node| {
            let mut text = String::new();
            collect_text(node, &mut text);
            text
        })
        .unwrap_or_default();
    let scoped: Vec<_> = root
        .descendants()
        .filter(|node| element_attr(node, "data-pagefind-body").is_some())
        .collect();
    let roots = if scoped.is_empty() {
        vec![element_named("body").unwrap_or(root)]
    } else {
        scoped
    };
    let mut body = String::new();
    for node in roots {
        collect_text(node, &mut body);
    }
    (title, body)
}

fn element_name<'a>(node: &NodeRef<'a, Node>) -> Option<&'a str> {
    match node.value() {
        Node::Element(element) => Some(element.name()),
        _ => None,
    }
}

fn element_attr<'a>(node: &NodeRef<'a, Node>, attribute: &str) -> Option<&'a str> {
    match node.value() {
        Node::Element(element) => element.attr(attribute),
        _ => None,
    }
}

/// Appends the text below `node`, leaving out hidden elements, with a space
/// at every element boundary so adjacent blocks do not run together.
fn collect_text(node: NodeRef<'_, Node>, out: &mut String) {
    match node.value() {
        Node::Text(text) => out.push_str(text),
        Node::Element(element) => {
            if HIDDEN_ELEMENTS.contains(&element.name())
                || element.attr("data-pagefind-ignore").is_some()
            {
                return;
            }
            out.push(' ');
            for child in node.children() {
                collect_text(child, out);
            }
            out.push(' ');
        }
        _ => {
            for child in node.children() {
                collect_text(child, out);
            }
        }
    }
}

/// The front matter's `title`, else the first `# heading`; and the prose
/// and code of the body, without the markup.
fn markdown(source: &str) -> (String, String) {
    let (front_matter, body) = markdown::split_front_matter(source);
    let mut title = front_matter
        .and_then(|(syntax, text)| markdown::parse_front_matter(syntax, text).ok())
        .and_then(|data| data.get("title")?.as_str().map(str::to_string));
    let options = Options::ENABLE_TABLES
        | Options::ENABLE_FOOTNOTES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_TASKLISTS
        | Options::ENABLE_HEADING_ATTRIBUTES
        | Options::ENABLE_GFM
        | Options::ENABLE_DEFINITION_LIST
        | Options::ENABLE_MATH;
    let mut text = String::new();
    let mut heading: Option<String> = None;
    for event in Parser::new_ext(body, options) {
        match event {
            Event::Start(Tag::Heading {
                level: HeadingLevel::H1,
                ..
            }) if title.is_none() => heading = Some(String::new()),
            Event::End(TagEnd::Heading(HeadingLevel::H1)) => {
                if let Some(found) = heading.take()
                    && title.is_none()
                    && !found.trim().is_empty()
                {
                    title = Some(found);
                }
                text.push(' ');
            }
            Event::Text(chunk) | Event::Code(chunk) => {
                if let Some(heading) = heading.as_mut() {
                    heading.push_str(&chunk);
                }
                text.push_str(&chunk);
            }
            Event::InlineMath(chunk) | Event::DisplayMath(chunk) => {
                text.push_str(&chunk);
                text.push(' ');
            }
            // Raw HTML in Markdown keeps its visible text, as a page would.
            Event::Html(chunk) | Event::InlineHtml(chunk) => {
                let fragment = Html::parse_fragment(&chunk);
                collect_text(fragment.tree.root(), &mut text);
            }
            Event::SoftBreak | Event::HardBreak | Event::Rule | Event::End(_) => text.push(' '),
            Event::Start(_) | Event::FootnoteReference(_) | Event::TaskListMarker(_) => {}
        }
    }
    (title.unwrap_or_default(), text)
}

/// The first non-empty line as the title, and the whole file as the text.
fn plain(source: &str) -> (String, String) {
    let mut title = source
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_string();
    truncate_chars(&mut title, 120);
    (title, source.to_string())
}

/// Collapses whitespace runs to one space and drops control characters and
/// the hit markers, so every stored field is one line that is safe in TSV
/// and unambiguous in snippets.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut pending_space = false;
    for character in text.chars() {
        if character.is_whitespace() {
            pending_space = !out.is_empty();
        } else if character.is_control() || matches!(character, HIT_START | HIT_END) {
            // dropped
        } else {
            if pending_space {
                out.push(' ');
                pending_space = false;
            }
            out.push(character);
        }
    }
    out
}

fn truncate_chars(text: &mut String, max: usize) {
    if let Some((at, _)) = text.char_indices().nth(max) {
        text.truncate(at);
    }
}

fn truncate_bytes(text: &mut String, max: usize) {
    if text.len() > max {
        let mut at = max;
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        text.truncate(at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn extracted(path: &str, source: &str) -> Document {
        extract(path, source.as_bytes()).expect("text is indexed")
    }

    #[test]
    fn html_keeps_visible_text_and_honours_pagefind_marks() {
        let page = r#"<!doctype html><html><head><title> Kan  extension </title>
            <style>h1 { color: red }</style><script>var x = "<p>no</p>";</script></head>
            <body><nav>Home · About</nav>
            <h1>Kan extension</h1><p>Why are Kan &amp; co <em>more</em> frequently<br>mentioned?</p>
            <svg><text>vector</text></svg><math><mi>x</mi></math>
            <footer data-pagefind-ignore>&copy; 2026</footer>
            <template><p>unused</p></template><!-- a comment --></body></html>"#;
        assert_eq!(
            extracted("a/index.html", page),
            Document {
                title: "Kan extension".to_string(),
                body: "Kan extension Why are Kan & co more frequently mentioned?".to_string(),
            }
        );
        let scoped = r"<html><body><header>Site chrome</header>
            <main data-pagefind-body><h1>Topos</h1><p>A topos is a category.</p></main>
            <aside data-pagefind-body>See also sheaf.</aside><footer>chrome</footer></body></html>";
        assert_eq!(
            extracted("topos.htm", scoped),
            Document {
                title: "Topos".to_string(),
                body: "Topos A topos is a category. See also sheaf.".to_string(),
            }
        );
        // Broken markup still yields its text.
        assert_eq!(
            extracted("x.html", "<p>one<p>two <b>three").body,
            "one two three"
        );
    }

    #[test]
    fn markdown_takes_the_front_matter_title_then_the_first_heading() {
        let source = "---\ntitle: Notes on sheaves\n---\n\n# Ignored heading\n\nA *sheaf* on `X` assigns data.\n\n```rust\nlet x = 1;\n```\n\n<div>raw <b>html</b> text</div>\n";
        assert_eq!(
            extracted("notes.md", source),
            Document {
                title: "Notes on sheaves".to_string(),
                body: "Ignored heading A sheaf on X assigns data. let x = 1; raw html text"
                    .to_string(),
            }
        );
        let untitled = "Intro line\n\n# First heading\n\nBody $x^2$ here.\n";
        let document = extracted("a.markdown", untitled);
        assert_eq!(document.title, "First heading");
        assert_eq!(document.body, "Intro line First heading Body x^2 here.");
    }

    #[test]
    fn other_text_files_are_taken_whole_with_their_first_line_as_title() {
        let document = extracted("data/config.json", "\n\n  {\"name\": \"demo\"}\n");
        assert_eq!(document.title, "{\"name\": \"demo\"}");
        assert_eq!(document.body, "{\"name\": \"demo\"}");
        let long = format!("{}\nrest", "x".repeat(500));
        assert_eq!(extracted("a.txt", &long).title.chars().count(), 120);
    }

    #[test]
    fn binary_and_empty_files_are_skipped_and_markers_are_stripped() {
        assert_eq!(extract("a.png", b"\x89PNG\r\n\x1a\n\0\0"), None);
        assert_eq!(extract("a.txt", b""), None);
        assert_eq!(extract("a.txt", &[0xff, 0xfe, b'a']), None);
        let document = extracted("a.txt", "a\u{E000}b\u{E001}c \t\n e");
        assert_eq!(document.body, "abc e");
        assert_eq!(clean("x\u{7}y\u{200B}z\u{E000}"), "xy\u{200B}z");
    }

    #[test]
    fn the_body_is_capped_at_a_char_boundary() {
        let source = "é".repeat(MAX_TEXT_BYTES);
        let document = extracted("a.txt", &source);
        assert!(document.body.len() <= MAX_TEXT_BYTES);
        assert!(document.body.ends_with('é'));
    }
}
