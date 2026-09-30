//! Rendered Markdown, cached by everything that determines it.
//!
//! A rendering is a pure function of the file's content hash, the name and
//! path it is served under (they fix the RAW link and the fallback title), and
//! the renderer itself. [`key`] hashes exactly those, so a changed file gets a
//! new key without any invalidation step, and the same key doubles as the
//! page's `ETag`: a browser revalidating an unchanged page is answered without
//! reading the file or rendering anything.
//!
//! The cache lives in memory and is bounded by bytes. Entries for superseded
//! content simply age out. A restart empties it, which is also the only time
//! the renderer can change.

use std::collections::{BTreeMap, HashMap};
use std::sync::{LazyLock, Mutex};

use axum::body::Bytes;

use crate::hash::ContentHash;

/// Upper bound on cached HTML across all pages.
const CAPACITY_BYTES: usize = 64 * 1024 * 1024;

/// A single page larger than this is rendered but not cached, so one huge
/// document cannot flush every other page.
const MAX_ENTRY_BYTES: usize = CAPACITY_BYTES / 8;

pub static CACHE: LazyLock<RenderCache> =
    LazyLock::new(|| RenderCache::new(CAPACITY_BYTES, MAX_ENTRY_BYTES));

/// Identifies the renderer, so a deploy that changes rendering changes every
/// key and a browser's old `ETag` stops matching.
///
/// The build's commit covers every change to any file, which is what makes
/// the tags safe across deploys: rendering also depends on code outside this
/// module. A commit alone misses edits in a dirty working tree, so the
/// rendering module, its assets and the exact dependency versions (the
/// Markdown parser among them) are hashed as well.
static RENDERER: LazyLock<blake3::Hash> = LazyLock::new(|| {
    renderer_identity(
        env!("SYMBOL_API_COMMIT"),
        env!("SYMBOL_API_DIRTY"),
        &[
            include_str!("markdown.rs"),
            include_str!("markdown_cache.rs"),
            crate::assets::BUNDLE.as_str(),
            include_str!("../../../Cargo.lock"),
        ],
    )
});

fn renderer_identity(commit: &str, dirty: &str, sources: &[&str]) -> blake3::Hash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"symbol-markdown-renderer-v2\0");
    for part in [commit, dirty].iter().chain(sources) {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    hasher.finalize()
}

pub type Key = [u8; 32];

/// The cache key, and `ETag`, for one file served at one path.
pub fn key(hash: ContentHash, name: &str, path: &str) -> Key {
    let mut hasher = blake3::Hasher::new();
    hasher.update(RENDERER.as_bytes());
    hasher.update(hash.as_bytes());
    for part in [name, path] {
        hasher.update(&(part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    *hasher.finalize().as_bytes()
}

/// A strong `ETag` for a rendering. The prefix keeps it visibly distinct from
/// the file's own `ETag`, which names the source bytes.
pub fn etag(key: &Key) -> String {
    format!("\"md-{}\"", blake3::Hash::from_bytes(*key).to_hex())
}

pub struct RenderCache {
    capacity: usize,
    max_entry: usize,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    entries: HashMap<Key, (Bytes, u64)>,
    /// Last use, oldest first, for eviction.
    recency: BTreeMap<u64, Key>,
    bytes: usize,
    clock: u64,
}

impl RenderCache {
    pub fn new(capacity: usize, max_entry: usize) -> Self {
        Self {
            capacity,
            max_entry: max_entry.min(capacity),
            state: Mutex::new(State::default()),
        }
    }

    pub fn get(&self, key: &Key) -> Option<Bytes> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.clock += 1;
        let now = state.clock;
        let (body, used) = state.entries.get_mut(key)?;
        let body = body.clone();
        let previous = std::mem::replace(used, now);
        state.recency.remove(&previous);
        state.recency.insert(now, *key);
        drop(state);
        Some(body)
    }

    pub fn insert(&self, key: Key, body: Bytes) {
        if body.len() > self.max_entry {
            return;
        }
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.clock += 1;
        let now = state.clock;
        if let Some((previous, used)) = state.entries.remove(&key) {
            state.bytes -= previous.len();
            state.recency.remove(&used);
        }
        while state.bytes + body.len() > self.capacity {
            let Some((_, oldest)) = state.recency.pop_first() else {
                break;
            };
            if let Some((evicted, _)) = state.entries.remove(&oldest) {
                state.bytes -= evicted.len();
            }
        }
        state.bytes += body.len();
        state.recency.insert(now, key);
        state.entries.insert(key, (body, now));
    }

    #[cfg(test)]
    fn contains(&self, key: &Key) -> bool {
        self.state.lock().unwrap().entries.contains_key(key)
    }
}

#[cfg(test)]
pub fn is_cached(key: &Key) -> bool {
    CACHE.contains(key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn content(label: &str) -> ContentHash {
        ContentHash::from_bytes(*blake3::hash(label.as_bytes()).as_bytes())
    }

    #[test]
    fn keys_change_with_content_name_and_path_only() {
        let base = key(content("a"), "site", "notes.md");
        assert_eq!(base, key(content("a"), "site", "notes.md"));
        assert_ne!(base, key(content("b"), "site", "notes.md"));
        assert_ne!(base, key(content("a"), "other", "notes.md"));
        assert_ne!(base, key(content("a"), "site", "other.md"));
        // Length prefixes keep the boundary between name and path unambiguous.
        assert_ne!(key(content("a"), "ab", "c"), key(content("a"), "a", "bc"));
        assert!(etag(&base).starts_with("\"md-"));
    }

    #[test]
    fn any_new_build_is_a_new_renderer() {
        let base = renderer_identity("abc123", "false", &["module", "assets"]);
        assert_eq!(
            base,
            renderer_identity("abc123", "false", &["module", "assets"])
        );
        // Another commit, even with the rendering sources untouched: the change
        // may be in code those sources call.
        assert_ne!(
            base,
            renderer_identity("def456", "false", &["module", "assets"])
        );
        // A dirty build of the same commit.
        assert_ne!(
            base,
            renderer_identity("abc123", "true", &["module", "assets"])
        );
        // An uncommitted edit to the renderer on the same dirty commit.
        assert_ne!(
            renderer_identity("abc123", "true", &["module", "assets"]),
            renderer_identity("abc123", "true", &["module edited", "assets"])
        );
        assert!(RENDERER.as_bytes() != &[0; 32]);
    }

    #[test]
    fn evicts_least_recently_used_within_the_byte_budget() {
        let cache = RenderCache::new(10, 10);
        let (a, b, c) = ([1; 32], [2; 32], [3; 32]);
        cache.insert(a, Bytes::from_static(b"aaaa"));
        cache.insert(b, Bytes::from_static(b"bbbb"));
        assert!(cache.get(&a).is_some(), "touch a so b is the oldest");
        cache.insert(c, Bytes::from_static(b"cccc"));
        assert!(cache.contains(&a));
        assert!(!cache.contains(&b), "b was least recently used");
        assert!(cache.contains(&c));

        cache.insert(a, Bytes::from_static(b"aaaaaa"));
        assert_eq!(cache.get(&a).unwrap(), "aaaaaa", "replaced in place");
        assert!(cache.state.lock().unwrap().bytes <= 10);
    }

    #[test]
    fn oversized_pages_are_not_cached() {
        let cache = RenderCache::new(100, 5);
        cache.insert([9; 32], Bytes::from_static(b"too long"));
        assert!(!cache.contains(&[9; 32]));
    }
}
