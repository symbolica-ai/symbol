//! Whether an uploaded HTML file should be served as `charset=utf-8`.
//!
//! An HTTP charset beats a page's own `<meta charset>` in every browser, so
//! labelling all HTML as UTF-8 would break a page that correctly declares
//! `Shift_JIS` or windows-1252. Leaving all HTML unlabelled breaks the far more
//! common page that is UTF-8 but never says so: the browser guesses, usually
//! windows-1252, and accents and emoji turn to mojibake.
//!
//! So decide per file, from its bytes, the way a browser would before it
//! parses: a page that declares its encoding (a byte order mark, or a `<meta>`
//! naming a charset within the first 1024 bytes) is left alone; a page that
//! declares nothing is labelled UTF-8 when its bytes are valid UTF-8, and left
//! to the browser otherwise. The verdict depends only on the content, so it is
//! cached by content hash.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::sync::{LazyLock, Mutex, PoisonError};

use crate::hash::ContentHash;

/// How far a browser looks for an encoding declaration before parsing.
const PRESCAN_BYTES: usize = 1024;

/// Verdicts kept before the cache starts over. One bool per entry, so even a
/// site of tens of thousands of pages stays well under a few megabytes.
const CACHE_ENTRIES: usize = 1 << 17;

static VERDICTS: LazyLock<Mutex<HashMap<ContentHash, bool>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Whether the HTML stored at `path`, with content hash `hash`, should be
/// served with `charset=utf-8`. Reads the file at most once per hash.
pub fn labels_as_utf8(hash: ContentHash, path: &Path) -> io::Result<bool> {
    if let Some(verdict) = lock().get(&hash) {
        return Ok(*verdict);
    }
    let verdict = classify(File::open(path)?)?;
    let mut verdicts = lock();
    if verdicts.len() >= CACHE_ENTRIES {
        verdicts.clear();
    }
    verdicts.insert(hash, verdict);
    drop(verdicts);
    Ok(verdict)
}

fn lock() -> std::sync::MutexGuard<'static, HashMap<ContentHash, bool>> {
    VERDICTS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The verdict for one document: no declaration of its own, and UTF-8 all the
/// way through.
fn classify(mut reader: impl Read) -> io::Result<bool> {
    let mut prefix = Vec::with_capacity(PRESCAN_BYTES);
    reader
        .by_ref()
        .take(PRESCAN_BYTES as u64)
        .read_to_end(&mut prefix)?;
    if declares_encoding(&prefix) {
        return Ok(false);
    }
    let mut validator = Utf8Validator::default();
    if !validator.feed(&prefix) {
        return Ok(false);
    }
    let mut chunk = vec![0; 64 * 1024];
    loop {
        let read = reader.read(&mut chunk)?;
        if read == 0 {
            return Ok(validator.finish());
        }
        if !validator.feed(&chunk[..read]) {
            return Ok(false);
        }
    }
}

/// Whether `prefix`, the start of a document, fixes its own encoding.
///
/// A UTF-16 byte order mark does, and so does any `<meta>` tag that mentions
/// `charset` outside a comment: that covers both `<meta charset=...>` and
/// `<meta http-equiv="Content-Type" content="...; charset=...">`. A UTF-8 BOM
/// is not counted: it agrees with the label this would add. Erring towards
/// "declared" only ever keeps the old, unlabelled behaviour.
fn declares_encoding(prefix: &[u8]) -> bool {
    if prefix.starts_with(&[0xFE, 0xFF]) || prefix.starts_with(&[0xFF, 0xFE]) {
        return true;
    }
    let mut rest = prefix;
    while let Some(start) = rest.iter().position(|&byte| byte == b'<') {
        rest = &rest[start..];
        if rest.starts_with(b"<!--") {
            // As the prescan does: the comment ends at the first `>` preceded
            // by `--`, counting from the `<`, so `<!-->` is already closed.
            match find(&rest[2..], b"-->") {
                Some(end) => rest = &rest[2 + end + 3..],
                None => return false,
            }
            continue;
        }
        let is_meta = rest.len() > 5
            && rest[1..5].eq_ignore_ascii_case(b"meta")
            && matches!(rest[5], b' ' | b'\t' | b'\n' | b'\x0c' | b'\r' | b'/');
        let end = rest
            .iter()
            .position(|&byte| byte == b'>')
            .unwrap_or(rest.len());
        if is_meta && find_ignore_case(&rest[..end], b"charset").is_some() {
            return true;
        }
        rest = &rest[end.max(1)..];
    }
    false
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn find_ignore_case(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

/// Checks UTF-8 across arbitrarily split chunks, carrying a character cut in
/// half at a chunk boundary over to the next.
#[derive(Default)]
struct Utf8Validator {
    carry: Vec<u8>,
}

impl Utf8Validator {
    fn feed(&mut self, chunk: &[u8]) -> bool {
        self.carry.extend_from_slice(chunk);
        match std::str::from_utf8(&self.carry) {
            Ok(_) => {
                self.carry.clear();
                true
            }
            // An incomplete sequence at the very end may finish next chunk.
            Err(error) if error.error_len().is_none() => {
                self.carry.drain(..error.valid_up_to());
                true
            }
            Err(_) => false,
        }
    }

    fn finish(self) -> bool {
        self.carry.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verdict(bytes: &[u8]) -> bool {
        classify(bytes).unwrap()
    }

    #[test]
    fn undeclared_utf8_is_labelled() {
        assert!(verdict(b"<!doctype html><title>plain</title><p>ascii only"));
        assert!(verdict("<p>café ✓ 🦀</p>".as_bytes()));
        assert!(verdict(b""));
        assert!(verdict(b"\xEF\xBB\xBF<p>UTF-8 BOM agrees</p>"));
    }

    #[test]
    fn a_declared_encoding_is_left_to_the_page() {
        assert!(!verdict(b"<meta charset=\"utf-8\"><p>caf\xC3\xA9"));
        assert!(!verdict(b"<META CHARSET=shift_jis><p>\x82\xA0"));
        assert!(!verdict(
            b"<meta http-equiv=\"Content-Type\" content=\"text/html; charset=windows-1252\">"
        ));
        assert!(!verdict(b"<meta\ncharset=utf-8>"));
        assert!(!verdict(b"\xFF\xFE<\0p\0>\0"));
        assert!(!verdict(b"\xFE\xFF\0<\0p\0>"));
    }

    #[test]
    fn undeclared_legacy_bytes_are_left_to_the_browser() {
        assert!(!verdict(b"<p>caf\xE9</p>"));
        assert!(!verdict(b"<p>truncated \xE2\x9C"));
    }

    #[test]
    fn only_real_meta_tags_count() {
        assert!(verdict(b"<!-- <meta charset=latin1> --><p>x"));
        assert!(!verdict(b"<!--><meta charset=latin1><p>x"));
        assert!(verdict(b"<metadata charset=x><p>x"));
        assert!(verdict(b"<p>charset is just a word here</p>"));
        // Everything after an unclosed `<!--` is comment, the tag included.
        assert!(verdict(b"<!-- unterminated comment <meta charset=latin1>"));
    }

    #[test]
    fn a_declaration_past_the_prescan_window_is_not_seen() {
        let mut page = vec![b' '; PRESCAN_BYTES];
        page.extend_from_slice(b"<meta charset=utf-8><p>late");
        assert!(verdict(&page));
    }

    #[test]
    fn characters_split_across_chunks_still_validate() {
        let text = "é✓🦀".repeat(40_000);
        assert!(verdict(text.as_bytes()));
        let mut validator = Utf8Validator::default();
        for byte in "🦀".bytes() {
            assert!(validator.feed(&[byte]));
        }
        assert!(validator.finish());

        let mut broken = text.into_bytes();
        broken.push(0xF0);
        assert!(!verdict(&broken));
    }

    #[test]
    fn verdicts_are_cached_by_hash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("page.html");
        std::fs::write(&path, "<p>café").unwrap();
        let hash = ContentHash::from(blake3::hash(b"verdicts_are_cached_by_hash"));
        assert!(labels_as_utf8(hash, &path).unwrap());
        std::fs::remove_file(&path).unwrap();
        assert!(labels_as_utf8(hash, &path).unwrap(), "read again");
    }
}
