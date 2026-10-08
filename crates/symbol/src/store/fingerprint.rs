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
    //! Known-answer tests. The fingerprint strings are persisted in SQLite
    //! idempotency records and compared on replay, so every expected value
    //! below was captured from the hand-rolled hashing code and must never
    //! change.

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

    const ALL_KINDS: [UndoKind; 12] = [
        UndoKind::Put,
        UndoKind::DeletePath,
        UndoKind::DeleteSite,
        UndoKind::Copy,
        UndoKind::Move,
        UndoKind::Expiry,
        UndoKind::ExpireSweep,
        UndoKind::PutFile,
        UndoKind::Allocate,
        UndoKind::Replace,
        UndoKind::Splice,
        UndoKind::Alias,
    ];

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

    fn expiries() -> Vec<FileExpiry> {
        vec![
            FileExpiry::Preserve,
            FileExpiry::Clear,
            FileExpiry::Policy(ExpiryPolicy::Relative {
                duration_seconds: 0,
            }),
            FileExpiry::Policy(ExpiryPolicy::Relative {
                duration_seconds: 3600,
            }),
            FileExpiry::Policy(ExpiryPolicy::Relative {
                duration_seconds: u64::MAX,
            }),
            FileExpiry::Policy(ExpiryPolicy::Absolute {
                deadline_unix_seconds: -1,
            }),
            FileExpiry::Policy(ExpiryPolicy::Absolute {
                deadline_unix_seconds: 1_900_000_000,
            }),
            FileExpiry::Policy(ExpiryPolicy::Absolute {
                deadline_unix_seconds: i64::MIN,
            }),
            FileExpiry::Policy(ExpiryPolicy::Decay(DecayPolicy {
                min_age_seconds: 60,
                max_age_seconds: 3600,
                max_size_bytes: 1 << 20,
                power: 1.0,
            })),
            FileExpiry::Policy(ExpiryPolicy::Decay(DecayPolicy {
                min_age_seconds: 0,
                max_age_seconds: u64::MAX,
                max_size_bytes: 1,
                power: 0.5,
            })),
            FileExpiry::Policy(ExpiryPolicy::Decay(DecayPolicy {
                min_age_seconds: 1,
                max_age_seconds: 2,
                max_size_bytes: 3,
                power: f64::NAN,
            })),
        ]
    }

    const HASH_FILE_EXPIRY: &[&str] = &[
        "2d3adedff11b61f14c886e35afa036736dcd87a74d27b5c1510225d0f592e213",
        "48fc721fbbc172e0925fa27af1671de225ba927134802998b10a1568a188652b",
        "c837765444815de6db1a10b88c56b546eb45d640b9323beb30f37038174a2c97",
        "b2b4cfcef1ea0355cd4de909c243d5720c3ccea05cbb7ccfc3407e2f4d091333",
        "5acd93b5b600ae916670a3358e7b25f6bf4ddfaa69728b71647043c19b138362",
        "26a748057ad47e1e96af29ea5b24000d8d896229cd73ba4d02d0ca0d702e7539",
        "da583414a5fab2b7c8cc29136005d6cac0f6fe088cd71c9ec29690582e965dd0",
        "56439446aa2533228b16b10ef665f19be7f12e8c1b6443ce8f5ce9e933bae3f6",
        "6424fffeb0e24fd0cc85caa3e6e4e247cfe0593da9646ea7b04b733462ec8b38",
        "c5907ab3e754b35cc5d5fdc041c459136f5b854c20e2b2cfce45db020badbe4c",
        "0be0af4f8478ba0a1379ddd9a6e8f8388bb1f5a1772cbeb3b722afb5aae5e244",
    ];

    #[test]
    fn hash_file_expiry_known_answers() {
        let actual: Vec<String> = expiries()
            .into_iter()
            .map(|expiry| {
                let mut hasher = blake3::Hasher::new();
                hash_file_expiry(&mut hasher, expiry);
                hasher.finalize().to_hex().to_string()
            })
            .collect();
        check("hash_file_expiry", &actual, HASH_FILE_EXPIRY);
    }

    const ENTRY_MUTATION: &[&str] = &[
        "f39ef57a6f6897e7a6c61f7429934af81a2e36e02eb9d759c880cdb68e946306",
        "f39ef57a6f6897e7a6c61f7429934af81a2e36e02eb9d759c880cdb68e946306",
        "5492f5f26eaf1f99b60cf5d82c534004be491077ee70041280969eb0eebedd7f",
        "4c2e2d70dc2ff172bced886ec077ab8c8a77fabd2c55c6287fc02debd818370b",
        "97352a81a4ff12ff8b6af1540332306293613edb509639106c7919fe7bb07f55",
        "52b7c38bea958f7277c12631883f29c71f77bdee2eda1bd8f5267b5ef0528740",
        "617c06de1fd270fe542f0c5252d3449d457c8980d00e7c9f2936d9d662093c73",
        "429d31035d3e0003ace1af384b4b52b409df471f00425cf9827afb2f7941c46c",
        "88d1351cf84d9aedd1c0865c7ce31680a74cc8fbca8498a47605aa15a96d589b",
        "205e5524df5f5a07c408b7e62596cc569dcd974ffa0164591c2ed1843bd0fa15",
        "37b89b9f199abf1e5b29d0904bb8336f17a89026d285bf5befb8ea9ab0b34966",
        "a527ea522d3d4346e61a68e0033dcd7dc0d5585787ffba0a47b60159213ddd7c",
        "c4db47f1cc0c557942a52b7959ababfdb0c710abc938c09f9803d940a384c8c5",
        "26dc9c5bee5bad3c09b358cb64acccbe4afe654d97c0b03e64ca618bd29e1f89",
        "160245d9133a534940536fbbdef28550d6410500f053ce935cabfdb49bf6ea66",
        "eeaca7748a8021b3ff111413fff866e2da23f37805cb3f209484975903383609",
        "bdeee46441013d9cd43e32028b1d9cf325badc656fc1615db7106d007f20c009",
        "9420f2d82c8dd198466d8914d38388bc8223d29649a8c604694a8d07fd7ec437",
        "bcc5f75d218c31486fc3ca7a367199c8d1a441b1c7f9155cb2850bc60bfe7ba2",
        "463615a122c1a146064aaea30aefa58c187604a7ecca9966d9ac30693b47fdf3",
    ];

    #[test]
    fn entry_mutation_fingerprint_known_answers() {
        let tree = hash_one().to_hex();
        let mut actual = vec![
            entry_mutation_fingerprint("", None, "", hash_zero(), UndoKind::Put, None),
            entry_mutation_fingerprint("", Some(""), "", hash_zero(), UndoKind::Put, Some("")),
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
            entry_mutation_fingerprint(
                UNICODE,
                Some(UNICODE),
                UNICODE,
                hash_one(),
                UndoKind::Alias,
                None,
            ),
            // Separator ambiguity: the NUL-separated fields share boundaries.
            entry_mutation_fingerprint("a", Some("b"), "c", hash_zero(), UndoKind::Put, None),
            entry_mutation_fingerprint("a\0b", None, "c", hash_zero(), UndoKind::Put, None),
            entry_mutation_fingerprint("a", Some("b\0c"), "", hash_zero(), UndoKind::Put, None),
        ];
        for kind in ALL_KINDS {
            actual.push(entry_mutation_fingerprint(
                "site",
                Some("p"),
                "q",
                hash_one(),
                kind,
                Some("tree"),
            ));
        }
        check("entry_mutation", &actual, ENTRY_MUTATION);
    }

    const CONTENT_MUTATION: &[&str] = &[
        "bdf0794e9a741b9c3d0676ec4ff93170cb3e9b23b4e83070aaac399c451fce0f",
        "bdf0794e9a741b9c3d0676ec4ff93170cb3e9b23b4e83070aaac399c451fce0f",
        "5c83c0340aa757af1b870a536bf6fce4bde74e9ab5af77c81a51ce948c6715a6",
        "481c8892f11a8d84f6fd1b2bcfe657a1403281f099e65e3ac09a606de200c11c",
        "20d5bfe83da65f57bd3a331197911fd56c5cdd4a25670ccaca2fb99be8565121",
        "ba620d916b54c62cc1ffa2e77c129d65382fce471985a517efa389010a41d7f6",
        "4b04d2dc3c4f53f1ee67f0577115a89f8bb055e88783c16f242d5f1b0075f588",
        "319e404ed750b548a1b48ed890b2dd432fada8c341f677a95e354216ffe9a72c",
        "13c0fc69e1ff8e6c0c725ba82426337dc1416855cebb50f94fbf29e7538343a9",
        "4cb70c9e742bcbb52eb1de3196f7ff6bec3ee44efa3f8f88d59f0411922f6f14",
        "206c220c53abd5255475580c70bd2f0fddabb921a174c0cc852f4a5bd2bea71e",
        "f01704b4fe7436738241c09493e052e53c26c5eec9b84553beadecddc66a8a54",
        "363b44639f8e83f3985b3d4952439be74a789bb4aa7f3753e359c55448dcaf0e",
        "bb74c98cef11a0824b387f3295d00fd867c89d5aa343533febb277dc3856a345",
        "af622bc60b10387af3df1e6ac57baea57998d177965c70f4fdd920ec4d5d0075",
        "2fab3dccde6ccc0ace3ed0f6ab9a7663b156d3e8fa0becf293abf9323d62b95d",
        "afc8564562ec1039278c3030f2ccbb57a4ea58d3fffbf6d947e7629131ed09cd",
        "70873b45418cf42572d334f3d872c8c54406c48c2eb047f91fbb06fc6d87eee6",
        "091c3dc96f33ad755d104c65709e5d86e6eb0ed1b3e249e86adfb4d9e43c32d9",
    ];

    #[test]
    fn content_mutation_request_fingerprint_known_answers() {
        let mut actual = vec![
            content_mutation_request_fingerprint("", "", hash_zero(), None, None, UndoKind::Put),
            content_mutation_request_fingerprint(
                "",
                "",
                hash_zero(),
                Some(""),
                Some(""),
                UndoKind::Put,
            ),
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
            content_mutation_request_fingerprint(
                UNICODE,
                UNICODE,
                hash_one(),
                None,
                Some(UNICODE),
                UndoKind::Splice,
            ),
            content_mutation_request_fingerprint("ab", "c", hash_zero(), None, None, UndoKind::Put),
            content_mutation_request_fingerprint("a", "bc", hash_zero(), None, None, UndoKind::Put),
        ];
        for kind in ALL_KINDS {
            actual.push(content_mutation_request_fingerprint(
                "site",
                "p",
                hash_one(),
                Some("c"),
                Some("t"),
                kind,
            ));
        }
        check("content_mutation_request", &actual, CONTENT_MUTATION);
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

    const STAGED_ENTRIES: &[&str] = &[
        "ad22097ac96455049f07ca62e2840aa26a4ccd074d8340f54521b77a61eb3973",
        "eb9086262b7dde473f9a60bc8204972430f24e6e44e8039d1f9ea697e7d3adc6",
        "50dd29b502001ba632dba662944c852fcde9d7d561e43b632f0a3c0046c243bd",
        "9b6b99ee7a9ed54e488ca769e7a4fafcecaa74f5cd3122e11ef245c0fd9d7af5",
        "9b6b99ee7a9ed54e488ca769e7a4fafcecaa74f5cd3122e11ef245c0fd9d7af5",
        "bb986765a9dc0ba28b6e21551fdfa004f7c835d9f7f7377edae898fb43d408a2",
        "1bda1766d15351d767293043a5c7c7047ecfd506d7ee796e29c2b632f03e21a7",
        "0ebb43abb0883e09f57410207f176853504f637b88d6c81abad22f86b3346dbd",
        "12c085d71cd130eab43c2d460cb78f04e431d836fc9d13b03b9945926dbcf091",
        "9820d604f5834266d2ff42a26d4765072af9f2a9b91da8c4b0b8aacd2243155f",
        "eaab2c818e6fff26299bb3bb0dbaad1f26552c6ef207f6e7aab882ee9715ca7b",
        "555ed37cd8c1a77abe0f02969872d9b1a3a2588ac6990c70d3f870047e344228",
    ];

    #[test]
    fn staged_entries_fingerprint_known_answers() {
        let empty = staged("", hash_zero());
        let a = staged("a.txt", hash_one());
        let b = staged("b/c.txt", hash_ones());
        let nul = staged("n\0ul", hash_zero());
        let uni = staged(UNICODE, hash_one());
        let alias = |path, target| ArchiveAlias { path, target };
        let actual = vec![
            staged_entries_fingerprint(&[], &[]),
            staged_entries_fingerprint(&[&empty], &[]),
            staged_entries_fingerprint(&[&a], &[]),
            staged_entries_fingerprint(&[&a, &b], &[]),
            // Input order is normalised by path.
            staged_entries_fingerprint(&[&b, &a], &[]),
            staged_entries_fingerprint(&[&uni, &nul, &b, &a, &empty], &[]),
            staged_entries_fingerprint(&[], &[alias("l", "t")]),
            staged_entries_fingerprint(&[], &[alias("", "")]),
            staged_entries_fingerprint(&[&a], &[alias("l", "a.txt")]),
            staged_entries_fingerprint(&[&a, &b], &[alias("l", "a.txt"), alias(UNICODE, "n\0ul")]),
            staged_entries_fingerprint(&[&a, &b], &[alias("n\0ul", UNICODE), alias("l", "a.txt")]),
            staged_entries_fingerprint(&[&nul], &[alias("\0alias\0", "x")]),
        ];
        check("staged_entries", &actual, STAGED_ENTRIES);
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

    const ALLOCATED_ENTRY_MUTATION: &[&str] = &[
        "6d9977dfc60bc2aa7baceefa0cf1080fa9aef4628af858a614031f2b25dec91b",
        "6d9977dfc60bc2aa7baceefa0cf1080fa9aef4628af858a614031f2b25dec91b",
        "5d2f2768636dcc42f7c2c5f1ad88b6ebf00715fa03c3a3aef5d0c710aeacc19b",
        "d9ef75b1b11d497db58806596d84bc8a7378a52f4e9d0d561f9222b69243c5bc",
        "6b2c6ebed9cae59fa8977372efd8c243e249ef5c0cca331165769d73425bdf91",
        "6b2c6ebed9cae59fa8977372efd8c243e249ef5c0cca331165769d73425bdf91",
        "f1ae13b7a6fcc4001b540238d2af88e6c453ae5cb02e27947168d0f4fb90274b",
        "ef49bce67d782d11535164a3b743c9cbe5355d8ddeb26cc91eefb27f8b564f14",
        "0d64e11e5afa40e0e4d7389cc75a1987541a1cce1758086651779d2f17b29901",
        "08dd81cd357288c1abedf393de72b56fa3176e4bc9f5d5af0d9aca898731f8df",
        "760f6e56c0620474a49a61d3b73a11ada372592dd80c6aa19281637bcef1f5bc",
        "dbc6433d7f9e727bea57a58c34b0acdf28217020bf1759df1ea85afd5c271da6",
        "cbb89611bcc5215465879a15eb8f623d6d357487f0545961b3ed20d054e87449",
        "b8c120f4a392fc159b5698a21b4c108b0f88e1665b79afae3de69dd94306b6f4",
        "5d7c9f331f77d83c1895a095d2724d2eb47d08d93402b7aa2b437fcbad13ec40",
        "def9465875fa5c2fd55e59658e3fe36ae027a004e9e2b93de1e008a8a99f4f77",
        "661f9845436b7b72ff855076f22872c6b12d565259007a6b9bd14daddeef5910",
        "d1a184f94b656e9e88555dc09e9b0148711f3ad2fc2dbe92621fe506f6f98534",
        "a2fd768815b80ccd1ce2ef0af9a4d2ba2757e59a7c402c544ae3553c1f11ebe0",
        "44f9b0936cdbb643e3ced7b74d6a9da8390a93a926281214c1f45eb7683a2b09",
        "2139c2b8f04c65aec0c3cca1b1b0e4929f756247bf3a64a52133e3eda673d8bb",
        "f1f8f5a87b2c8d34cfe211d79b9ac8cf74965b933884bda16fc2752e45275a2c",
        "766c4543efd60e30edc8b87dcef12d77b841e59e2e394750d2b9b47442a67b79",
        "9a75bcadc70a5da591979e66f3e383d919d950e63d382bae168b19c1586c7c68",
        "c0df5ae17154239eda5af4eecb03988d163d5ceb7aa58597f7d65bd5dba95ac2",
        "195bc4af4a5a26835cd7e824f119e6f8fc3441097cb3a0a01d5fd9922cd192c7",
        "49602c03afdb6784a03bef569a0683f498b10e75428ea0f9513a935c0f97efb9",
        "376521db615cdf201ee1d65793b6a844b4a592b3099b4f175dc83ccae273cfa1",
        "d529fcbe5f3373576a6f2eae4340afde8ddc81542064edd30a2813487984d44f",
        "ff09a672c63f30aca5faac2f00c2ca1990866c49a4c9b69e55129701a7b1d09c",
        "c827bd5dd917ae5bb92e5012113dc5c37e68e4a58ad1c510f97dd79695bfab94",
        "32dd32fea3bb22ead16a288a95312b6ca9f26cea055a46cd1212b12935da164f",
        "232795241916e4bd7a20172d9e6eed9e0340a327f9b533ad14589974b0ad8533",
    ];

    #[test]
    fn allocated_entry_mutation_fingerprint_known_answers() {
        let modes = [
            AllocatedNamingMode::ContentAddressed,
            AllocatedNamingMode::Custom,
        ];
        let mut actual = Vec::new();
        for naming_mode in modes {
            for extension in [None, Some(""), Some("png"), Some("t\0x")] {
                let destination = destination(naming_mode, extension, "d");
                actual.push(allocated_entry_mutation_fingerprint(
                    &AllocatedEntryFingerprint {
                        name: "site",
                        current_path: Some("a/b"),
                        destination: &destination,
                        hash: hash_one(),
                        kind: UndoKind::Allocate,
                        expiry: FileExpiry::Preserve,
                        expected_tree_hash: Some("tree"),
                    },
                ));
            }
        }
        let uni = destination(AllocatedNamingMode::Custom, Some(UNICODE), UNICODE);
        let nul = destination(AllocatedNamingMode::ContentAddressed, None, "\0");
        actual.push(allocated_entry_mutation_fingerprint(
            &AllocatedEntryFingerprint {
                name: "",
                current_path: None,
                destination: &nul,
                hash: hash_zero(),
                kind: UndoKind::Put,
                expiry: FileExpiry::Clear,
                expected_tree_hash: None,
            },
        ));
        actual.push(allocated_entry_mutation_fingerprint(
            &AllocatedEntryFingerprint {
                name: UNICODE,
                current_path: Some(""),
                destination: &uni,
                hash: hash_ones(),
                kind: UndoKind::Replace,
                expiry: FileExpiry::Preserve,
                expected_tree_hash: Some(""),
            },
        ));
        for kind in ALL_KINDS {
            actual.push(allocated_entry_mutation_fingerprint(
                &AllocatedEntryFingerprint {
                    name: "n",
                    current_path: Some("c"),
                    destination: &uni,
                    hash: hash_one(),
                    kind,
                    expiry: FileExpiry::Preserve,
                    expected_tree_hash: None,
                },
            ));
        }
        for expiry in expiries() {
            actual.push(allocated_entry_mutation_fingerprint(
                &AllocatedEntryFingerprint {
                    name: "n",
                    current_path: Some("c"),
                    destination: &nul,
                    hash: hash_one(),
                    kind: UndoKind::Allocate,
                    expiry,
                    expected_tree_hash: Some("t"),
                },
            ));
        }
        check(
            "allocated_entry_mutation",
            &actual,
            ALLOCATED_ENTRY_MUTATION,
        );
    }

    const PENDING: &[&str] = &[
        "fd6263dd29e1e263319bdad3daf71c3bc0991971833655099461c8583a2e31b4",
        "fd6263dd29e1e263319bdad3daf71c3bc0991971833655099461c8583a2e31b4",
        "68febb967182945ed5cf54d0a48d938c4e62fb274848bfd1da06122194a84806",
        "167c269f53bcf0a3fce1d03ddabdd1262954acc90bf7ea841a56714c16408ac9",
        "f2d9e96a429fcec0cc6b35ceafc2f5a1bf88f18374cbb8c0bb45369b0c7c96a4",
        "59c3b3c0c0493a9bd18e5fe342bec862eaab882b17ae3ec331533953dac94743",
        "56f01aaf4cbab2ddbe6f70d90da8fdbabde14001f27b865e4b6d230626cf0f3d",
        "ce5d7bd5e28fd5309a074d9a8a49e1225975e68cc17e9447aa128e0449ecfa11",
        "68febb967182945ed5cf54d0a48d938c4e62fb274848bfd1da06122194a84806",
        "f943c942c2da9ed4f58346142a427766d21770ab7910b9f35bffa180ade46cd1",
        "2658adce5602e5a81017edf6e07bdae6fb860000a1f06c763c099883afb363b3",
        "c126243dbe81b80406ef21b957c1cd181fb45cb169e71da1923526b46516ff62",
        "8fb2ca49c68590de957cc679de5f21056c917b8d019471b5a1b45e8a8c26fd2b",
        "e372ca2c9da089a2cd92cf51509904a1309fad66951e9f01ae67a2db3494f37b",
        "9d43b21fd8df8ad424546a6d3a82cd0d0a8b434264e2c94049e61b82c4701a2b",
        "18e70ec17ba09a7acedad4e3f90ae7d2d0512ba6d762e458a9141c8805443612",
        "b871a7b2a8ba9753a69e45f13da1105dbcb3e58c4e6921449dc75d63c7a2f270",
        "11c6429c69fa376196b2c0febc3211369bea8103ff4811972e487e8701e23f40",
        "658f69b88701be58f9a0e72eedca1d6c737233b155475129507e752ea6c806fc",
    ];

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
        let mut actual = vec![
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
            pending_fingerprint(&PendingFingerprint {
                authorization_hash: Some(""),
                extension: Some(""),
                expected_tree_hash: Some(""),
                ..PendingFingerprint {
                    site: "",
                    folder: "",
                    hash: hash_zero(),
                    content_size: 0,
                    media_type: "",
                    expiry: FileExpiry::Preserve,
                    authorization_hash: None,
                    extension: None,
                    expected_tree_hash: None,
                }
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
            pending_fingerprint(&PendingFingerprint {
                site: UNICODE,
                folder: UNICODE,
                media_type: UNICODE,
                authorization_hash: Some(UNICODE),
                extension: Some(UNICODE),
                expected_tree_hash: Some(UNICODE),
                ..base
            }),
            pending_fingerprint(&PendingFingerprint {
                authorization_hash: None,
                ..base
            }),
            pending_fingerprint(&PendingFingerprint {
                extension: None,
                ..base
            }),
            pending_fingerprint(&PendingFingerprint {
                expected_tree_hash: None,
                ..base
            }),
        ];
        for expiry in expiries() {
            actual.push(pending_fingerprint(&PendingFingerprint { expiry, ..base }));
        }
        check("pending", &actual, PENDING);
    }

    const LEGACY_PENDING: &[&str] = &[
        "acce31e25b2efdc19d0d24c6f257fcb4b033b8e53594316c2975f76d4a9e8ed7",
        "2b3c560755b3b5d27c23675cc988b6e42b6ab19d497df49a64e6ed7528d82969",
        "64e31adfb02a4525f9d3eb91dbc80cbafc8e38acefa3a1525d7002242c4f8348",
        "a6402689a8e69c75b9462ddd4e7ca248b8a0abb3b10a6d2dbeb472b42c5df195",
        "e7bdef1af1e7e72822c3cc1c4f71333cf6ce82b0759d6a359b43431ca45336cc",
        "b4792c29e2e93c8a6a92bd80d7d070c908925d7aeb631550b8b72ef9b530d824",
        "7910dda616c0ec60646bde8d65c18856d3dff856b47290841ef7d6cdc16f069b",
    ];

    #[test]
    fn legacy_pending_fingerprint_known_answers() {
        let actual = vec![
            legacy_pending_fingerprint("", "", hash_zero(), 0, ""),
            legacy_pending_fingerprint("site", "f/g", hash_one(), 1234, "image/png"),
            legacy_pending_fingerprint("a\0b", "\0", hash_ones(), u64::MAX, "x\0y"),
            legacy_pending_fingerprint(UNICODE, UNICODE, hash_one(), 1, UNICODE),
            legacy_pending_fingerprint("ab", "c", hash_one(), 7, "d"),
            legacy_pending_fingerprint("a", "bc", hash_one(), 7, "d"),
            legacy_pending_fingerprint("a", "b", hash_one(), 256, "cd"),
        ];
        check("legacy_pending", &actual, LEGACY_PENDING);
    }

    const CANCELLATION: &[&str] = &[
        "5103b293e32c0d556432d61b25532e0ce618aee9badd52cd6631a44869e4a256",
        "5103b293e32c0d556432d61b25532e0ce618aee9badd52cd6631a44869e4a256",
        "083080dd09fd6c1fdff317e11a72ab83c04124c7ccd6fae279913a728884b1f4",
        "b6ceafeb22003c1066ab58381045009c27678ba068c28656549cdc28b3b71829",
        "61c9e7fc013bb53d1bbd15d39337d8728e5ce4d36a9c94cf55a5c66f98d6e0b0",
        "49a35908c8aa58e402d36e6ba6abf4b65d769edb621e1df133088589547494c3",
        "9866ff04e16501d743e184beae529aea0abf54c06fc580203aca2367f135cc17",
        "04beb6ef899b3e98641b8b75be45fc1a8e70292b4afb4baea6956aa44c88fb53",
        "56d34589276699a9c62d50dd79b9e4323a4a7a4f6400c5aafebb6c090ddf1ba2",
    ];

    #[test]
    fn cancellation_fingerprint_known_answers() {
        let actual = vec![
            cancellation_fingerprint("", "", "", None, None),
            cancellation_fingerprint("", "", "", Some(""), Some("")),
            cancellation_fingerprint("site", "token", "f/g", Some("tree"), Some("auth")),
            cancellation_fingerprint("site", "token", "f/g", None, Some("auth")),
            cancellation_fingerprint("site", "token", "f/g", Some("tree"), None),
            cancellation_fingerprint("a\0b", "\0", "c\0", Some("\0"), Some("a\0")),
            cancellation_fingerprint(UNICODE, UNICODE, UNICODE, Some(UNICODE), Some(UNICODE)),
            cancellation_fingerprint("ab", "c", "", None, None),
            cancellation_fingerprint("a", "bc", "", None, None),
        ];
        check("cancellation", &actual, CANCELLATION);
    }

    const PENDING_FINALIZE: &[&str] = &[
        "4a1f8d4f9a4f932d644b4d8222acb0fc3f0ee62b529de105ce855bd01afa993c",
        "7728def0a7e0047f16994d896140570b638707ea84cdfa4f7b9f22af6c1fc9fc",
        "92fd09deb2c78d70c03dd0ecaee526b106aa556fbbae47098e352af6f2dd88f8",
        "4faf2615aa4f2af2f2568af30d480fece517d89424b58710af91bfd41d657903",
        "bef7e13b82dc17b6461acf7bedc6616fc269233621fc891419e48cadff8f5d42",
        "426a2378407a79d5a732a1cf68a11aa86caac7e05ba34e7d8a400d9b328e9580",
        "9ca18f940d18961e02bd86709082091a7fa0f5e60c4621d6c6546860b8d97e7d",
        "310a25e1b293816f6359c5aa5f9cc97d4844bcbed4829fb8245625100a6f191c",
        "1ac19298758f6bb29c9d66466463d767fffb6fd2c5fa0143a8ae04114dca8868",
        "2611c0e266f50e2e6e36cd823cbf1e9d646cd3fb30fa63b14d9f16d93dcb4d20",
        "22630ad2d60c25191f76821032be0b0e85bcf51ec1b78d510d0912e6ddf7bcd4",
        "92fd09deb2c78d70c03dd0ecaee526b106aa556fbbae47098e352af6f2dd88f8",
        "24a32ea5559b8c7693126308df23576b3a6a7801b59ed8ef1c36a424183f6b47",
        "8ba349f99a59004682cc7979f17577994a5b4fd12916283c2622e50b835ca960",
        "88bacda0b77ee4dac1f1da441fd3aab3dfc7c85625336cebf5bc3b0894299c75",
        "55e0649cd9a94a15512b65d7ebd675d0f9b3389db434b0b53613b942acb3d1b7",
        "56ea4acef251cd1e9c7526ea1eb5703d1f22e72a24c26c6cd1086ae99d58bbac",
        "580cee64a377c39dadfea141ff9d4fdc56208911e9f6276d1a1c34592968f62b",
        "aa791a0dce2741482176990d41a730b2cfec607f89bd40305f22fef8ea034111",
        "8e1c688d21c669fb5f4f4285341a876c7cb77321909be6a9e5cad03566192f22",
        "9597fa5a6d6d4f37bee0c1485c93f808f12439b4e613662070390001f18d2129",
        "d258338a7e1ded00d7d26b4041f6a8e3b447fc7b3d4e011be0cfd060aa9b0ff7",
    ];

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
        let generated = |prefix, suffix, extension| {
            PendingFinalName::Generated(AllocatedName {
                prefix,
                suffix,
                extension,
            })
        };
        let mut actual = vec![
            pending_finalize_fingerprint(&PendingFinalizeFingerprint {
                site: "",
                token: "",
                folder: None,
                final_name: PendingFinalName::Custom(""),
                expiry: FileExpiry::Preserve,
                authorization_hash: None,
                expected_tree_hash: None,
            }),
            pending_finalize_fingerprint(&PendingFinalizeFingerprint {
                site: "",
                token: "",
                folder: Some(""),
                final_name: generated("", "", Some("")),
                expiry: FileExpiry::Preserve,
                authorization_hash: Some(""),
                expected_tree_hash: Some(""),
            }),
            pending_finalize_fingerprint(&base),
            pending_finalize_fingerprint(&PendingFinalizeFingerprint {
                folder: None,
                authorization_hash: None,
                expected_tree_hash: None,
                ..base
            }),
            pending_finalize_fingerprint(&PendingFinalizeFingerprint {
                final_name: generated("pre", "suf", Some("png")),
                ..base
            }),
            pending_finalize_fingerprint(&PendingFinalizeFingerprint {
                final_name: generated("pre", "suf", None),
                ..base
            }),
            pending_finalize_fingerprint(&PendingFinalizeFingerprint {
                final_name: generated("a\0b", "\0", Some("e\0")),
                site: "s\0",
                token: "\0t",
                folder: Some("\0"),
                ..base
            }),
            pending_finalize_fingerprint(&PendingFinalizeFingerprint {
                final_name: generated(UNICODE, UNICODE, Some(UNICODE)),
                site: UNICODE,
                token: UNICODE,
                folder: Some(UNICODE),
                authorization_hash: Some(UNICODE),
                expected_tree_hash: Some(UNICODE),
                ..base
            }),
            pending_finalize_fingerprint(&PendingFinalizeFingerprint {
                final_name: PendingFinalName::Custom(UNICODE),
                ..base
            }),
            // Generated and custom names must not collide on shared bytes.
            pending_finalize_fingerprint(&PendingFinalizeFingerprint {
                final_name: PendingFinalName::Custom("ab"),
                ..base
            }),
            pending_finalize_fingerprint(&PendingFinalizeFingerprint {
                final_name: generated("a", "b", None),
                ..base
            }),
        ];
        for expiry in expiries() {
            actual.push(pending_finalize_fingerprint(&PendingFinalizeFingerprint {
                expiry,
                ..base
            }));
        }
        check("pending_finalize", &actual, PENDING_FINALIZE);
    }

    const ALIAS_MUTATION: &[&str] = &[
        "18f0e7bae7b49dcba6d796d6b6cb2d4692e97a5e948859fe5476b78ac516b657",
        "18f0e7bae7b49dcba6d796d6b6cb2d4692e97a5e948859fe5476b78ac516b657",
        "9909dd5e297ebef5ec549f15fd1eb51c043b23f6b5dd2f7f0b7ee2ceed7870ef",
        "46bcc12dc46d9e5b460d56f58b8a1eb82c40e1179a6576ee97b907fb1fb4b0a0",
        "8262845949c841c4960f7529f9b1f60819398d0c8efecfe28c8cd0b97d72e6aa",
        "f9f99b17ff8928de7b47e94f047a02afeaaf4d48775ddacb43423e341c3332bf",
        "6ac823c2597a23a9ac5d4080475b2a49b6e338fe9e01a7b3a2d98db1a0e9c9e1",
        "503e726bc1c2fe2ff92d8aec4a2a16734c20ce8d87bd9ce60152d861fb337030",
        "7a97699b41eb0c506e97d16affc1eee82af5c0d3078de13308a3be89d31b20c0",
        "409f2b0f49aef6943067ff3b6915caf1f471d7eaf4a0fbeaea6b88ca672b0453",
    ];

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
            alias_mutation_fingerprint("", &map(&[]), Some("")),
            alias_mutation_fingerprint("site", &map(&[]), Some("tree")),
            alias_mutation_fingerprint("site", &map(&[("l", "t")]), None),
            alias_mutation_fingerprint("site", &map(&[("", "")]), None),
            alias_mutation_fingerprint(
                "site",
                &map(&[("b/l", "../t"), ("a/l", "t/u"), ("c", "d")]),
                Some("tree"),
            ),
            alias_mutation_fingerprint("a\0b", &map(&[("p\0", "\0q"), ("\0", "\0")]), Some("t\0")),
            alias_mutation_fingerprint(UNICODE, &map(&[(UNICODE, UNICODE)]), Some(UNICODE)),
            // The name is raw, so these differ only through the length prefixes.
            alias_mutation_fingerprint("ab", &map(&[("c", "d")]), None),
            alias_mutation_fingerprint("a", &map(&[("bc", "d")]), None),
        ];
        check("alias_mutation", &actual, ALIAS_MUTATION);
    }
}
