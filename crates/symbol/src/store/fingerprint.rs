//! Request fingerprints are persisted in SQLite idempotency records and
//! compared byte-for-byte on replay, so the exact byte stream fed to the hash
//! is part of the storage format. `Fingerprint` names the two framings used
//! (NUL-terminated and u64-length-prefixed) so each call site says which one
//! it uses; the known-answer tests below pin the resulting digests.

use super::{FileExpiry, hash_file_expiry};

/// A `blake3` hasher seeded with a domain tag.
pub(super) struct Fingerprint(blake3::Hasher);

impl Fingerprint {
    /// Start with `domain` followed by a NUL, e.g. `symbol-entry-mutation-v1\0`.
    pub(super) fn new(domain: &str) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(domain.as_bytes());
        hasher.update(&[0]);
        Self(hasher)
    }

    /// Bytes exactly as given, with no framing.
    pub(super) fn raw(mut self, bytes: impl AsRef<[u8]>) -> Self {
        self.0.update(bytes.as_ref());
        self
    }

    /// The value's bytes followed by a NUL.
    pub(super) fn separated(self, value: &str) -> Self {
        self.raw(value).raw([0])
    }

    /// The value's byte length as a little-endian `u64`, then its bytes.
    pub(super) fn len_prefixed(self, value: &str) -> Self {
        self.u64_le(value.len() as u64).raw(value)
    }

    /// A single discriminant byte.
    pub(super) fn tag(self, tag: u8) -> Self {
        self.raw([tag])
    }

    pub(super) fn u64_le(self, value: u64) -> Self {
        self.raw(value.to_le_bytes())
    }

    pub(super) fn i64_le(self, value: i64) -> Self {
        self.raw(value.to_le_bytes())
    }

    pub(super) fn expiry(mut self, expiry: FileExpiry) -> Self {
        hash_file_expiry(&mut self.0, expiry);
        self
    }

    pub(super) fn finish(self) -> String {
        self.0.finalize().to_hex().to_string()
    }
}

#[cfg(test)]
mod tests {
    //! Regression tripwires for the fingerprint encodings. The strings are
    //! persisted in SQLite idempotency records (which live only for
    //! `IDEMPOTENCY_RETENTION_MILLIS`, 4 hours) and compared on replay, so an
    //! accidental change to the byte stream would make in-flight retries stop
    //! matching. Each function keeps a few vectors recorded from the original
    //! hand-rolled hashing code: the empty input, a populated one, and one
    //! sensitive to NUL/length framing. If you change an encoding on purpose,
    //! regenerate the expected values; this is not a permanent format contract.

    use std::collections::BTreeMap;

    use crate::expiry::{DecayPolicy, ExpiryPolicy};
    use crate::hash::ContentHash;
    use crate::sanitize::TokenCounts;
    use crate::store::{
        AllocatedEntryFingerprint, AllocatedName, AllocatedNamingMode, AllocationDestination,
        ArchiveAlias, FileExpiry, PendingFinalName, PendingFinalizeFingerprint, PendingFingerprint,
        StagedFile, StagedSource, UndoKind, alias_mutation_fingerprint,
        allocated_entry_mutation_fingerprint, cancellation_fingerprint,
        content_mutation_request_fingerprint, entry_mutation_fingerprint, hash_file_expiry,
        legacy_pending_fingerprint, pending_finalize_fingerprint, pending_fingerprint,
        staged_entries_fingerprint,
    };

    fn check(name: &str, actual: &[String], expected: &[&str]) {
        assert_eq!(actual, expected, "{name} fingerprints changed");
    }

    const UNICODE: &str = "h\u{e9}llo/\u{65e5}\u{672c}\u{8a9e}/\u{1f980}";

    fn hash_zero() -> ContentHash {
        ContentHash::from([0_u8; 32])
    }

    fn hash_one() -> ContentHash {
        ContentHash::from(blake3::hash(b"one"))
    }

    fn hash_ones() -> ContentHash {
        ContentHash::from([0xff_u8; 32])
    }

    #[test]
    fn hash_file_expiry_known_answers() {
        let expiries = [
            FileExpiry::Preserve,
            FileExpiry::Policy(ExpiryPolicy::Relative {
                duration_seconds: 3600,
            }),
            FileExpiry::Policy(ExpiryPolicy::Absolute {
                deadline_unix_seconds: 1_900_000_000,
            }),
            FileExpiry::Policy(ExpiryPolicy::Decay(DecayPolicy {
                min_age_seconds: 60,
                max_age_seconds: 3600,
                max_size_bytes: 1 << 20,
                power: 1.0,
            })),
        ];
        let actual: Vec<String> = expiries
            .into_iter()
            .map(|expiry| {
                let mut hasher = blake3::Hasher::new();
                hash_file_expiry(&mut hasher, expiry);
                hasher.finalize().to_hex().to_string()
            })
            .collect();
        check(
            "hash_file_expiry",
            &actual,
            &[
                "2d3adedff11b61f14c886e35afa036736dcd87a74d27b5c1510225d0f592e213",
                "b2b4cfcef1ea0355cd4de909c243d5720c3ccea05cbb7ccfc3407e2f4d091333",
                "da583414a5fab2b7c8cc29136005d6cac0f6fe088cd71c9ec29690582e965dd0",
                "6424fffeb0e24fd0cc85caa3e6e4e247cfe0593da9646ea7b04b733462ec8b38",
            ],
        );
    }

    #[test]
    fn entry_mutation_fingerprint_known_answers() {
        let tree = hash_one().to_hex();
        let actual = vec![
            entry_mutation_fingerprint("", None, "", hash_zero(), UndoKind::Put, None),
            entry_mutation_fingerprint(
                "site",
                Some("a/b.txt"),
                "a/c.txt",
                hash_one(),
                UndoKind::Move,
                Some(&tree),
            ),
            entry_mutation_fingerprint(
                "a\0b",
                Some("\0"),
                "c\0d",
                hash_ones(),
                UndoKind::Copy,
                Some("t\0"),
            ),
        ];
        check(
            "entry_mutation",
            &actual,
            &[
                "f39ef57a6f6897e7a6c61f7429934af81a2e36e02eb9d759c880cdb68e946306",
                "5492f5f26eaf1f99b60cf5d82c534004be491077ee70041280969eb0eebedd7f",
                "4c2e2d70dc2ff172bced886ec077ab8c8a77fabd2c55c6287fc02debd818370b",
            ],
        );
    }

    #[test]
    fn content_mutation_request_fingerprint_known_answers() {
        let actual = vec![
            content_mutation_request_fingerprint("", "", hash_zero(), None, None, UndoKind::Put),
            content_mutation_request_fingerprint(
                "site",
                "a/b.txt",
                hash_one(),
                Some("old"),
                Some("tree"),
                UndoKind::Replace,
            ),
            content_mutation_request_fingerprint(
                "a\0b",
                "\0",
                hash_ones(),
                Some("c\0"),
                None,
                UndoKind::PutFile,
            ),
        ];
        check(
            "content_mutation_request",
            &actual,
            &[
                "bdf0794e9a741b9c3d0676ec4ff93170cb3e9b23b4e83070aaac399c451fce0f",
                "5c83c0340aa757af1b870a536bf6fce4bde74e9ab5af77c81a51ce948c6715a6",
                "481c8892f11a8d84f6fd1b2bcfe657a1403281f099e65e3ac09a606de200c11c",
            ],
        );
    }

    fn staged(path: &str, hash: ContentHash) -> StagedFile {
        StagedFile {
            path: path.to_string(),
            size: 0,
            hash,
            source: StagedSource::Bytes(Vec::new()),
            sanitized: TokenCounts::default(),
        }
    }

    #[test]
    fn staged_entries_fingerprint_known_answers() {
        let a = staged("a.txt", hash_one());
        let b = staged("b/c.txt", hash_ones());
        let alias = |path, target| ArchiveAlias { path, target };
        let actual = vec![
            staged_entries_fingerprint(&[], &[]),
            // Input order is normalised by path.
            staged_entries_fingerprint(&[&b, &a], &[]),
            staged_entries_fingerprint(&[&a, &b], &[alias("l", "a.txt"), alias(UNICODE, "n\0ul")]),
        ];
        check(
            "staged_entries",
            &actual,
            &[
                "ad22097ac96455049f07ca62e2840aa26a4ccd074d8340f54521b77a61eb3973",
                "9b6b99ee7a9ed54e488ca769e7a4fafcecaa74f5cd3122e11ef245c0fd9d7af5",
                "9820d604f5834266d2ff42a26d4765072af9f2a9b91da8c4b0b8aacd2243155f",
            ],
        );
    }

    fn destination(
        naming_mode: AllocatedNamingMode,
        extension: Option<&str>,
        text: &str,
    ) -> AllocationDestination {
        AllocationDestination {
            path: format!("{text}/path"),
            naming_mode,
            prefix: format!("{text}-pre"),
            suffix: format!("{text}-suf"),
            extension: extension.map(str::to_string),
            media_type: format!("text/{text}"),
        }
    }

    #[test]
    fn allocated_entry_mutation_fingerprint_known_answers() {
        let nul = destination(AllocatedNamingMode::ContentAddressed, None, "\0");
        let png = destination(AllocatedNamingMode::Custom, Some("png"), "d");
        let nul_ext = destination(AllocatedNamingMode::Custom, Some("t\0x"), "d");
        let populated = |destination| AllocatedEntryFingerprint {
            name: "site",
            current_path: Some("a/b"),
            destination,
            hash: hash_one(),
            kind: UndoKind::Allocate,
            expiry: FileExpiry::Preserve,
            expected_tree_hash: Some("tree"),
        };
        let actual = vec![
            allocated_entry_mutation_fingerprint(&AllocatedEntryFingerprint {
                name: "",
                current_path: None,
                destination: &nul,
                hash: hash_zero(),
                kind: UndoKind::Put,
                expiry: FileExpiry::Clear,
                expected_tree_hash: None,
            }),
            allocated_entry_mutation_fingerprint(&populated(&png)),
            allocated_entry_mutation_fingerprint(&populated(&nul_ext)),
        ];
        check(
            "allocated_entry_mutation",
            &actual,
            &[
                "0d64e11e5afa40e0e4d7389cc75a1987541a1cce1758086651779d2f17b29901",
                "f1ae13b7a6fcc4001b540238d2af88e6c453ae5cb02e27947168d0f4fb90274b",
                "ef49bce67d782d11535164a3b743c9cbe5355d8ddeb26cc91eefb27f8b564f14",
            ],
        );
    }

    #[test]
    fn pending_fingerprint_known_answers() {
        let base = PendingFingerprint {
            site: "site",
            folder: "f/g",
            hash: hash_one(),
            content_size: 1234,
            media_type: "image/png",
            expiry: FileExpiry::Preserve,
            authorization_hash: Some("auth"),
            extension: Some("png"),
            expected_tree_hash: Some("tree"),
        };
        let actual = vec![
            pending_fingerprint(&PendingFingerprint {
                site: "",
                folder: "",
                hash: hash_zero(),
                content_size: 0,
                media_type: "",
                expiry: FileExpiry::Preserve,
                authorization_hash: None,
                extension: None,
                expected_tree_hash: None,
            }),
            pending_fingerprint(&base),
            pending_fingerprint(&PendingFingerprint {
                site: "a\0b",
                folder: "\0",
                media_type: "x\0y",
                content_size: u64::MAX,
                hash: hash_ones(),
                ..base
            }),
        ];
        check(
            "pending",
            &actual,
            &[
                "fd6263dd29e1e263319bdad3daf71c3bc0991971833655099461c8583a2e31b4",
                "68febb967182945ed5cf54d0a48d938c4e62fb274848bfd1da06122194a84806",
                "167c269f53bcf0a3fce1d03ddabdd1262954acc90bf7ea841a56714c16408ac9",
            ],
        );
    }

    #[test]
    fn legacy_pending_fingerprint_known_answers() {
        let actual = vec![
            legacy_pending_fingerprint("", "", hash_zero(), 0, ""),
            legacy_pending_fingerprint("site", "f/g", hash_one(), 1234, "image/png"),
            legacy_pending_fingerprint("a\0b", "\0", hash_ones(), u64::MAX, "x\0y"),
        ];
        check(
            "legacy_pending",
            &actual,
            &[
                "acce31e25b2efdc19d0d24c6f257fcb4b033b8e53594316c2975f76d4a9e8ed7",
                "2b3c560755b3b5d27c23675cc988b6e42b6ab19d497df49a64e6ed7528d82969",
                "64e31adfb02a4525f9d3eb91dbc80cbafc8e38acefa3a1525d7002242c4f8348",
            ],
        );
    }

    #[test]
    fn cancellation_fingerprint_known_answers() {
        let actual = vec![
            cancellation_fingerprint("", "", "", None, None),
            cancellation_fingerprint("site", "token", "f/g", Some("tree"), Some("auth")),
            cancellation_fingerprint("a\0b", "\0", "c\0", Some("\0"), Some("a\0")),
        ];
        check(
            "cancellation",
            &actual,
            &[
                "5103b293e32c0d556432d61b25532e0ce618aee9badd52cd6631a44869e4a256",
                "083080dd09fd6c1fdff317e11a72ab83c04124c7ccd6fae279913a728884b1f4",
                "49a35908c8aa58e402d36e6ba6abf4b65d769edb621e1df133088589547494c3",
            ],
        );
    }

    #[test]
    fn pending_finalize_fingerprint_known_answers() {
        let base = PendingFinalizeFingerprint {
            site: "site",
            token: "token",
            folder: Some("f/g"),
            final_name: PendingFinalName::Custom("name.txt"),
            expiry: FileExpiry::Preserve,
            authorization_hash: Some("auth"),
            expected_tree_hash: Some("tree"),
        };
        let actual = vec![
            pending_finalize_fingerprint(&PendingFinalizeFingerprint {
                site: "",
                token: "",
                folder: None,
                final_name: PendingFinalName::Custom(""),
                expiry: FileExpiry::Preserve,
                authorization_hash: None,
                expected_tree_hash: None,
            }),
            pending_finalize_fingerprint(&base),
            // A generated name, which is framed differently from a custom one.
            pending_finalize_fingerprint(&PendingFinalizeFingerprint {
                final_name: PendingFinalName::Generated(AllocatedName {
                    prefix: "a\0b",
                    suffix: "\0",
                    extension: Some("e\0"),
                }),
                site: "s\0",
                token: "\0t",
                folder: Some("\0"),
                ..base
            }),
        ];
        check(
            "pending_finalize",
            &actual,
            &[
                "4a1f8d4f9a4f932d644b4d8222acb0fc3f0ee62b529de105ce855bd01afa993c",
                "92fd09deb2c78d70c03dd0ecaee526b106aa556fbbae47098e352af6f2dd88f8",
                "9ca18f940d18961e02bd86709082091a7fa0f5e60c4621d6c6546860b8d97e7d",
            ],
        );
    }

    #[test]
    fn alias_mutation_fingerprint_known_answers() {
        let map = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(path, target)| ((*path).to_string(), (*target).to_string()))
                .collect()
        };
        let actual = vec![
            alias_mutation_fingerprint("", &map(&[]), None),
            alias_mutation_fingerprint(
                "site",
                &map(&[("b/l", "../t"), ("a/l", "t/u"), ("c", "d")]),
                Some("tree"),
            ),
            alias_mutation_fingerprint("a\0b", &map(&[("p\0", "\0q"), ("\0", "\0")]), Some("t\0")),
        ];
        check(
            "alias_mutation",
            &actual,
            &[
                "18f0e7bae7b49dcba6d796d6b6cb2d4692e97a5e948859fe5476b78ac516b657",
                "f9f99b17ff8928de7b47e94f047a02afeaaf4d48775ddacb43423e341c3332bf",
                "6ac823c2597a23a9ac5d4080475b2a49b6e338fe9e01a7b3a2d98db1a0e9c9e1",
            ],
        );
    }
}
