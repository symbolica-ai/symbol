//! Built-in assets that rendered pages reference.
//!
//! Served from `/ASSETS/{bundle}/{path}`. `{bundle}` is a digest of every asset
//! below, so any change to any of them moves every URL. That is what makes it
//! safe to serve them `immutable` for a year: a browser holding a stale copy is
//! never asked for it again, because the page that referenced it now names a
//! different bundle. A request naming any other bundle is `404` rather than a
//! silent substitution, which would break that promise.
//!
//! KaTeX and highlight.js are fetched into `static/vendor/` rather than
//! committed; `static/vendor.toml` pins their versions, tarball hashes and
//! licences, and lists the files taken.

use std::sync::LazyLock;

pub struct Asset {
    pub path: &'static str,
    pub content_type: &'static str,
    pub bytes: &'static [u8],
}

macro_rules! asset {
    ($path:expr, $file:expr, $content_type:expr) => {
        Asset {
            path: $path,
            content_type: $content_type,
            bytes: include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../static/", $file)),
        }
    };
}

macro_rules! katex_font {
    ($name:literal) => {
        asset!(
            concat!("katex/fonts/", $name),
            concat!("vendor/katex/fonts/", $name),
            "font/woff2"
        )
    };
}

const CSS: &str = "text/css; charset=utf-8";
const JS: &str = "text/javascript; charset=utf-8";

pub static ASSETS: &[Asset] = &[
    asset!("markdown.css", "markdown.css", CSS),
    asset!("markdown.js", "markdown.js", JS),
    asset!("katex/katex.min.css", "vendor/katex/katex.min.css", CSS),
    asset!("katex/katex.min.js", "vendor/katex/katex.min.js", JS),
    asset!("katex/copy-tex.min.js", "vendor/katex/copy-tex.min.js", JS),
    asset!(
        "highlight/highlight.min.js",
        "vendor/highlight/highlight.min.js",
        JS
    ),
    katex_font!("KaTeX_AMS-Regular.woff2"),
    katex_font!("KaTeX_Caligraphic-Bold.woff2"),
    katex_font!("KaTeX_Caligraphic-Regular.woff2"),
    katex_font!("KaTeX_Fraktur-Bold.woff2"),
    katex_font!("KaTeX_Fraktur-Regular.woff2"),
    katex_font!("KaTeX_Main-Bold.woff2"),
    katex_font!("KaTeX_Main-BoldItalic.woff2"),
    katex_font!("KaTeX_Main-Italic.woff2"),
    katex_font!("KaTeX_Main-Regular.woff2"),
    katex_font!("KaTeX_Math-BoldItalic.woff2"),
    katex_font!("KaTeX_Math-Italic.woff2"),
    katex_font!("KaTeX_SansSerif-Bold.woff2"),
    katex_font!("KaTeX_SansSerif-Italic.woff2"),
    katex_font!("KaTeX_SansSerif-Regular.woff2"),
    katex_font!("KaTeX_Script-Regular.woff2"),
    katex_font!("KaTeX_Size1-Regular.woff2"),
    katex_font!("KaTeX_Size2-Regular.woff2"),
    katex_font!("KaTeX_Size3-Regular.woff2"),
    katex_font!("KaTeX_Size4-Regular.woff2"),
    katex_font!("KaTeX_Typewriter-Regular.woff2"),
];

/// Digest of the whole asset set; the `{bundle}` path segment.
pub static BUNDLE: LazyLock<String> = LazyLock::new(|| {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"symbol-render-assets-v1\0");
    for asset in ASSETS {
        for part in [
            asset.path.as_bytes(),
            asset.content_type.as_bytes(),
            asset.bytes,
        ] {
            hasher.update(&(part.len() as u64).to_le_bytes());
            hasher.update(part);
        }
    }
    hasher.finalize().to_hex()[..16].to_string()
});

/// The URL prefix every rendered page uses for its assets.
pub fn base() -> String {
    format!("/ASSETS/{}", BUNDLE.as_str())
}

/// Resolves a request, but only against the bundle this binary serves.
pub fn find(bundle: &str, path: &str) -> Option<&'static Asset> {
    if bundle != BUNDLE.as_str() {
        return None;
    }
    ASSETS.iter().find(|asset| asset.path == path)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::Path;

    use super::*;

    #[test]
    fn every_vendored_katex_font_is_served() {
        // katex.min.css names each font by relative URL, so one missing here
        // is a silently broken glyph rather than a compile error.
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../static/vendor/katex/fonts");
        let on_disk: BTreeSet<String> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| {
                format!(
                    "katex/fonts/{}",
                    entry.unwrap().file_name().to_string_lossy()
                )
            })
            .collect();
        let served: BTreeSet<String> = ASSETS
            .iter()
            .map(|asset| asset.path.to_string())
            .filter(|path| path.starts_with("katex/fonts/"))
            .collect();
        assert_eq!(served, on_disk);
    }

    #[test]
    fn every_font_the_stylesheet_prefers_exists() {
        let css = std::str::from_utf8(find(&BUNDLE, "katex/katex.min.css").unwrap().bytes).unwrap();
        let mut referenced = 0;
        for piece in css.split("url(fonts/").skip(1) {
            let name = piece.split(')').next().unwrap();
            if Path::new(name)
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("woff2"))
            {
                referenced += 1;
                assert!(
                    find(&BUNDLE, &format!("katex/fonts/{name}")).is_some(),
                    "katex.min.css prefers {name}, which is not served"
                );
            }
        }
        assert!(
            referenced >= 20,
            "expected KaTeX's full font set, saw {referenced}"
        );
    }

    #[test]
    fn only_the_current_bundle_resolves() {
        assert!(find(&BUNDLE, "markdown.css").is_some());
        assert!(find("0000000000000000", "markdown.css").is_none());
        assert!(find(&BUNDLE, "../Cargo.toml").is_none());
        assert!(find(&BUNDLE, "missing.css").is_none());
        assert_eq!(BUNDLE.len(), 16);
        assert!(base().starts_with("/ASSETS/"));
    }

    #[test]
    fn assets_are_unique_and_typed() {
        let mut seen = BTreeSet::new();
        for asset in ASSETS {
            assert!(seen.insert(asset.path), "duplicate {}", asset.path);
            assert!(!asset.bytes.is_empty(), "empty {}", asset.path);
            let expected = match asset.path.rsplit('.').next().unwrap() {
                "css" => CSS,
                "js" => JS,
                "woff2" => "font/woff2",
                other => panic!("untyped extension {other}"),
            };
            assert_eq!(asset.content_type, expected, "{}", asset.path);
        }
    }
}
