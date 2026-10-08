#[derive(Debug)]
pub struct EndpointContract {
    pub name: &'static str,
    pub method: &'static str,
    pub head: bool,
    pub path: &'static str,
    pub request_headers: &'static [&'static str],
    pub outcomes: &'static [EndpointOutcome],
    // Endpoint-owned so legacy endpoint summaries remain derived and byte-stable.
    summary_headers: &'static [&'static str],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeKind {
    Success,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestVariant {
    Default,
    Create,
    Finalize,
    Propose,
    Cancel,
}

impl RequestVariant {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Create => "create",
            Self::Finalize => "finalize",
            Self::Propose => "propose",
            Self::Cancel => "cancel",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointOutcome {
    pub kind: OutcomeKind,
    pub status: u16,
    pub request_variant: RequestVariant,
    pub body: WireBody,
    pub schema: WireSchema,
    pub required_headers: &'static [&'static str],
    pub optional_headers: &'static [&'static str],
    pub forbidden_headers: &'static [&'static str],
}

impl EndpointContract {
    #[must_use]
    pub fn exact_outcome(
        &self,
        kind: OutcomeKind,
        status: u16,
        request_variant: RequestVariant,
    ) -> Option<&EndpointOutcome> {
        self.outcomes.iter().find(|outcome| {
            outcome.kind == kind
                && outcome.status == status
                && outcome.request_variant == request_variant
        })
    }

    #[must_use]
    pub fn outcome(
        &self,
        status: u16,
        request_variant: RequestVariant,
    ) -> Option<&EndpointOutcome> {
        self.outcomes
            .iter()
            .find(|outcome| outcome.status == status && outcome.request_variant == request_variant)
            .or_else(|| {
                self.outcomes.iter().find(|outcome| {
                    outcome.status == status && outcome.request_variant == RequestVariant::Default
                })
            })
    }

    #[must_use]
    pub fn has_status(&self, status: u16) -> bool {
        self.outcomes.iter().any(|outcome| outcome.status == status)
    }

    #[must_use]
    pub fn response_headers(&self) -> Vec<&'static str> {
        let mut headers = Vec::new();
        for header in self.summary_headers {
            if !headers.contains(header) {
                headers.push(*header);
            }
        }
        headers
    }
}

impl serde::Serialize for EndpointContract {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct as _;
        let mut endpoint = serializer.serialize_struct("EndpointContract", 8)?;
        endpoint.serialize_field("name", self.name)?;
        endpoint.serialize_field("method", self.method)?;
        endpoint.serialize_field("head", &self.head)?;
        endpoint.serialize_field("path", self.path)?;
        endpoint.serialize_field(
            "success_statuses",
            &status_summary(self, OutcomeKind::Success),
        )?;
        endpoint.serialize_field("error_statuses", &status_summary(self, OutcomeKind::Error))?;
        endpoint.serialize_field("request_headers", self.request_headers)?;
        endpoint.serialize_field("response_headers", &self.response_headers())?;
        endpoint.end()
    }
}

fn status_summary(endpoint: &EndpointContract, kind: OutcomeKind) -> Vec<u16> {
    let mut statuses = Vec::new();
    for outcome in endpoint
        .outcomes
        .iter()
        .filter(|outcome| outcome.kind == kind)
    {
        if !statuses.contains(&outcome.status) {
            statuses.push(outcome.status);
        }
    }
    statuses
}

pub const ROOT: &str = "/";
pub const HASH: &str = "/HASH";
pub const STATS: &str = "/STATS";
pub const STATS_SLASH: &str = "/STATS/";
pub const FILES: &str = "/FILES";
pub const FILES_SLASH: &str = "/FILES/";
pub const INSTALL: &str = "/install.sh";
pub const INSTALL_HASH: &str = "/install.sh/HASH";
pub const CLIENT: &str = "/symbol.sh";
pub const CLIENT_HASH: &str = "/symbol.sh/HASH";
pub const API_TS: &str = "/symbol.ts";
pub const API_TS_HASH: &str = "/symbol.ts/HASH";
pub const API_JS: &str = "/symbol.js";
pub const API_JS_HASH: &str = "/symbol.js/HASH";
pub const API_GLOBAL_JS: &str = "/symbol.global.js";
pub const API_GLOBAL_JS_HASH: &str = "/symbol.global.js/HASH";
pub const API_D_TS: &str = "/symbol.d.ts";
pub const API_D_TS_HASH: &str = "/symbol.d.ts/HASH";
pub const API_PY: &str = "/symbol.py";
pub const API_PY_HASH: &str = "/symbol.py/HASH";
pub const API: &str = "/API";
pub const API_INDEX: &str = "/API/";
pub const API_JS_MANUAL: &str = "/API/JS";
pub const API_TS_MANUAL: &str = "/API/TS";
pub const API_PY_MANUAL: &str = "/API/PY";
pub const API_PYTHON_MANUAL: &str = "/API/PYTHON";
pub const API_SH_MANUAL: &str = "/API/SH";
pub const API_CURL_MANUAL: &str = "/API/CURL";
pub const API_HTTP_MANUAL: &str = "/API/HTTP";
pub const API_REST_MANUAL: &str = "/API/REST";
pub const API_PROTOCOL_MANUAL: &str = "/API/PROTOCOL";
pub const API_MARKDOWN_MANUAL: &str = "/API/MARKDOWN";
pub const API_MD_MANUAL: &str = "/API/MD";
pub const API_MARKDOWN_RAW: &str = "/API/MARKDOWN/RAW";
pub const API_VERSION: &str = "/API/VERSION";
pub const API_PATH: &str = "/API/{*path}";
pub const SITE_FILES: &str = "/{name}/FILES";
pub const SITE_FILES_SLASH: &str = "/{name}/FILES/";
pub const SITE_FILES_PATH: &str = "/{name}/FILES/{*path}";
pub const SITE_UNDO: &str = "/{name}/UNDO";
pub const SITE_UNDO_SLASH: &str = "/{name}/UNDO/";
pub const SITE_EXPIRES: &str = "/{name}/EXPIRES";
pub const SITE_EXPIRES_SLASH: &str = "/{name}/EXPIRES/";
pub const SITE_ROOT: &str = "/{name}/";
pub const IMMUTABLE_BLOB: &str = "/.blob/{name}/{hash}";
pub const RENDER_ASSET: &str = "/ASSETS/{bundle}/{*path}";
pub const SITE_PATH: &str = "/{name}/{*path}";
pub const SITE: &str = "/{name}";

pub const METHOD_ALIAS: &str = "ALIAS";
pub const METHOD_COPY: &str = "COPY";
pub const METHOD_REPLACE: &str = "REPLACE";
pub const METHOD_MOVE: &str = "MOVE";
pub const METHOD_UNDO: &str = "UNDO";
pub const METHOD_EXPIRE: &str = "EXPIRE";
pub const METHOD_MANAGE: &str = "MANAGE";
pub const RESERVED_MUTATION_ERROR: &str = "error: path is reserved by symbol";
pub const SPLICE_MEDIA_TYPE: &str = "application/vnd.symbol.splice; version=1";
pub const SPLICE_HEADER_MAX_BYTES: usize = 8 * 1024;
pub const SPLICE_HEADER_MAX_DESCRIPTORS: usize = 64;
pub const SPLICE_FRAME_MAX_DESCRIPTORS: usize = 4096;
pub const SPLICE_FRAME_MAX_METADATA_BYTES: usize = 96 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WireBody {
    Empty,
    Json,
    PlainText,
    Binary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WireSchema {
    Empty,
    Asset,
    ApiVersion,
    Hash,
    Stats,
    DirectoryListing,
    HostedContent,
    SiteMutationReceipt,
    Archive,
    UndoMutationReceipt,
    ExpiryReport,
    ManagementStatus,
    FileInventory,
    UndoStack,
    ExpirySiteReport,
    AliasReceipt,
    AliasBatchReceipt,
    AllocationReceipt,
    AllocationProposalReceipt,
    AllocationCancellationReceipt,
    FileReplaceReceipt,
    SpliceReceipt,
    Error,
}

impl EndpointOutcome {
    const fn new(
        kind: OutcomeKind,
        status: u16,
        body: WireBody,
        schema: WireSchema,
        required_headers: &'static [&'static str],
    ) -> Self {
        Self {
            kind,
            status,
            request_variant: RequestVariant::Default,
            body,
            schema,
            required_headers,
            optional_headers: &[],
            forbidden_headers: FORBID_RANGE_LOCATION,
        }
    }

    const fn variant(mut self, request_variant: RequestVariant) -> Self {
        self.request_variant = request_variant;
        self
    }

    const fn error(mut self) -> Self {
        self.kind = OutcomeKind::Error;
        self
    }

    const fn optional(mut self, headers: &'static [&'static str]) -> Self {
        self.optional_headers = headers;
        self
    }

    const fn retry_after(self) -> Self {
        self.optional(&["Retry-After"])
    }

    const fn forbid(mut self, headers: &'static [&'static str]) -> Self {
        self.forbidden_headers = headers;
        self
    }
}

const fn text(
    status: u16,
    schema: WireSchema,
    required: &'static [&'static str],
) -> EndpointOutcome {
    EndpointOutcome::new(
        OutcomeKind::Success,
        status,
        WireBody::PlainText,
        schema,
        required,
    )
}

const fn json(
    status: u16,
    schema: WireSchema,
    required: &'static [&'static str],
) -> EndpointOutcome {
    EndpointOutcome::new(
        OutcomeKind::Success,
        status,
        WireBody::Json,
        schema,
        required,
    )
}

const fn binary(
    status: u16,
    schema: WireSchema,
    required: &'static [&'static str],
) -> EndpointOutcome {
    EndpointOutcome::new(
        OutcomeKind::Success,
        status,
        WireBody::Binary,
        schema,
        required,
    )
}

const fn empty(
    kind: OutcomeKind,
    status: u16,
    required: &'static [&'static str],
) -> EndpointOutcome {
    EndpointOutcome::new(kind, status, WireBody::Empty, WireSchema::Empty, required)
}

/// Plain-text `Error` body that only requires `Content-Type`.
const fn err(status: u16) -> EndpointOutcome {
    EndpointOutcome::new(
        OutcomeKind::Error,
        status,
        WireBody::PlainText,
        WireSchema::Error,
        CONTENT_TYPE,
    )
}

const fn unauthorized() -> EndpointOutcome {
    EndpointOutcome::new(
        OutcomeKind::Error,
        401,
        WireBody::PlainText,
        WireSchema::Error,
        &["Content-Type", "WWW-Authenticate", "Cache-Control"],
    )
}

const fn precondition_failed() -> EndpointOutcome {
    EndpointOutcome::new(
        OutcomeKind::Error,
        412,
        WireBody::PlainText,
        WireSchema::Error,
        &["Content-Type", "ETag", "Content-Revision"],
    )
}

/// Empty conditional-request response; defaults to the `Success` kind.
const fn not_modified() -> EndpointOutcome {
    empty(OutcomeKind::Success, 304, &["ETag", "Cache-Control"]).forbid(&[
        "Content-Type",
        "Content-Length",
        "Content-Range",
        "Location",
    ])
}

const fn redirect(required: &'static [&'static str]) -> EndpointOutcome {
    empty(OutcomeKind::Success, 307, required).forbid(&["Content-Type", "Content-Range", "ETag"])
}

const fn range_not_satisfiable() -> EndpointOutcome {
    empty(
        OutcomeKind::Error,
        416,
        &["Content-Range", "Accept-Ranges", "ETag", "Cache-Control"],
    )
    .forbid(&["Content-Type", "Location"])
}

// Required/optional/forbidden header lists shared by many outcomes.
const CONTENT_TYPE: &[&str] = &["Content-Type"];
const CONTENT_TYPE_CACHE: &[&str] = &["Content-Type", "Cache-Control"];
const ASSET_HEADERS: &[&str] = &["Content-Type", "ETag", "Cache-Control"];
const RECEIPT_HEADERS: &[&str] = &["Content-Type", "Location", "ETag", "Content-Revision"];
const ALLOCATION_HEADERS: &[&str] = &[
    "Content-Type",
    "Content-Location",
    "Location",
    "ETag",
    "Content-Revision",
];
const UNDO_HEADERS: &[&str] = &["Content-Type", "Undo-Token", "Undo-Expires"];
const ARCHIVE_UNDO_HEADERS: &[&str] = &[
    "Content-Type",
    "Content-Length",
    "Content-Disposition",
    "Undo-Token",
    "Undo-Expires",
];
const HOSTED_HEADERS: &[&str] = &[
    "Content-Type",
    "Content-Length",
    "ETag",
    "Cache-Control",
    "Accept-Ranges",
];
const PARTIAL_HEADERS: &[&str] = &[
    "Content-Type",
    "Content-Length",
    "Content-Range",
    "Accept-Ranges",
    "ETag",
    "Cache-Control",
];
const PAGINATION_HEADERS: &[&str] = &["Entry-Count", "Link"];
const FORBID_RANGE_LOCATION: &[&str] = &["Content-Range", "Location"];
const FORBID_RANGE: &[&str] = &["Content-Range"];
const FORBID_RANGE_REPLAYED: &[&str] = &["Content-Range", "Idempotency-Replayed"];
const FORBID_RANGE_CONTENT_LOCATION: &[&str] = &["Content-Range", "Content-Location"];
const FORBID_RANGE_ETAG_LOCATION: &[&str] = &["Content-Range", "ETag", "Location"];
const FORBID_RANGE_LOCATION_ETAG: &[&str] = &["Content-Range", "Location", "ETag"];

// Endpoint-level response header summaries (ordering is part of the API ledger).
const SUMMARY_CONTENT_TYPE: &[&str] = &["Content-Type"];
const SUMMARY_CONTENT_TYPE_CACHE: &[&str] = &["Content-Type", "Cache-Control"];
const SUMMARY_PAGE: &[&str] = &["Content-Type", "ETag", "Cache-Control"];
const SUMMARY_ASSET: &[&str] = &["Cache-Control", "Content-Type", "ETag"];
const SUMMARY_FILE: &[&str] = &[
    "Content-Type",
    "Content-Length",
    "Content-Range",
    "Accept-Ranges",
    "ETag",
    "Cache-Control",
    "Expires",
];
const SUMMARY_POP: &[&str] = &[
    "Content-Type",
    "Content-Length",
    "Content-Disposition",
    "Undo-Token",
    "Undo-Expires",
];
const SUMMARY_EXPIRE: &[&str] = &[
    "Content-Type",
    "Expires",
    "Expiry-Mode",
    "Undo-Token",
    "Undo-Expires",
];
const SUMMARY_MUTATION: &[&str] = &[
    "Content-Type",
    "Location",
    "ETag",
    "Content-Revision",
    "Undo-Token",
    "Undo-Expires",
    "Creator-Claim",
    "Management-Token",
    "Sanitized-Management-Tokens",
    "Sanitized-Creator-Claims",
];
const SUMMARY_CREATE_MUTATION: &[&str] = &[
    "Content-Type",
    "Location",
    "ETag",
    "Content-Revision",
    "Undo-Token",
    "Undo-Expires",
    "Creator-Claim",
    "Management-Token",
    "Sanitized-Management-Tokens",
    "Sanitized-Creator-Claims",
    "Idempotency-Replayed",
];
const SUMMARY_RECEIPT: &[&str] = &[
    "Content-Revision",
    "Content-Type",
    "ETag",
    "Idempotency-Replayed",
    "Location",
    "Undo-Expires",
    "Undo-Token",
];

pub static ENDPOINTS: &[EndpointContract] = &[
    EndpointContract {
        name: "docs",
        method: "GET",
        head: true,
        path: "/",
        request_headers: &["Accept", "If-None-Match"],
        summary_headers: SUMMARY_PAGE,
        outcomes: &[
            text(200, WireSchema::Asset, ASSET_HEADERS),
            not_modified(),
            not_modified().error(),
        ],
    },
    EndpointContract {
        name: "unnamed put",
        method: "PUT",
        head: false,
        path: "/",
        request_headers: &[
            "Content-Type",
            "Content-Disposition",
            "Unpack",
            "Idempotency-Key",
            "Creator-Claim",
            "Management-Action",
        ],
        summary_headers: SUMMARY_CREATE_MUTATION,
        outcomes: &[
            text(201, WireSchema::SiteMutationReceipt, RECEIPT_HEADERS).forbid(FORBID_RANGE),
            err(400),
            unauthorized(),
            err(403),
            err(404),
            err(409),
            precondition_failed(),
            err(413),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "docs hash",
        method: "GET",
        head: true,
        path: "/HASH",
        request_headers: &[],
        summary_headers: SUMMARY_CONTENT_TYPE,
        outcomes: &[text(200, WireSchema::Hash, CONTENT_TYPE), err(404)],
    },
    EndpointContract {
        name: "stats",
        method: "GET",
        head: true,
        path: "/STATS[/]",
        request_headers: &[],
        summary_headers: SUMMARY_CONTENT_TYPE,
        outcomes: &[
            json(200, WireSchema::Stats, CONTENT_TYPE),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "installer",
        method: "GET",
        head: true,
        path: "/install.sh",
        request_headers: &["If-None-Match"],
        summary_headers: SUMMARY_PAGE,
        outcomes: &[
            text(200, WireSchema::Asset, ASSET_HEADERS),
            not_modified(),
            not_modified().error(),
        ],
    },
    EndpointContract {
        name: "installer hash",
        method: "GET",
        head: true,
        path: "/install.sh/HASH",
        request_headers: &[],
        summary_headers: SUMMARY_CONTENT_TYPE,
        outcomes: &[text(200, WireSchema::Hash, CONTENT_TYPE), err(404)],
    },
    EndpointContract {
        name: "client",
        method: "GET",
        head: true,
        path: "/symbol.sh",
        request_headers: &["If-None-Match"],
        summary_headers: SUMMARY_PAGE,
        outcomes: &[
            text(200, WireSchema::Asset, ASSET_HEADERS),
            not_modified(),
            not_modified().error(),
        ],
    },
    EndpointContract {
        name: "client hash",
        method: "GET",
        head: true,
        path: "/symbol.sh/HASH",
        request_headers: &[],
        summary_headers: SUMMARY_CONTENT_TYPE,
        outcomes: &[text(200, WireSchema::Hash, CONTENT_TYPE), err(404)],
    },
    EndpointContract {
        name: "api client asset",
        method: "GET",
        head: true,
        path: "/{symbol.ts|symbol.js|symbol.global.js|symbol.d.ts|symbol.py|api.ts|api.js|api.global.js|api.d.ts|api.py}",
        request_headers: &["If-None-Match"],
        summary_headers: SUMMARY_ASSET,
        outcomes: &[
            text(200, WireSchema::Asset, ASSET_HEADERS),
            not_modified(),
            not_modified().error(),
        ],
    },
    EndpointContract {
        name: "api client hash",
        method: "GET",
        head: true,
        path: "/{symbol.ts|symbol.js|symbol.global.js|symbol.d.ts|symbol.py|api.ts|api.js|api.global.js|api.d.ts|api.py}/HASH",
        request_headers: &[],
        summary_headers: SUMMARY_CONTENT_TYPE,
        outcomes: &[text(200, WireSchema::Hash, CONTENT_TYPE), err(404)],
    },
    EndpointContract {
        name: "api documentation",
        method: "GET",
        head: true,
        path: "/API[/]|/API/{JS|TS|PY|PYTHON|SH|CURL|HTTP|REST|PROTOCOL|MARKDOWN|MD}|/API/MARKDOWN/RAW",
        request_headers: &["Accept", "If-None-Match"],
        summary_headers: &[
            "Cache-Control",
            "Content-Type",
            "ETag",
            "Link",
            "Location",
            "Vary",
        ],
        outcomes: &[
            text(200, WireSchema::Asset, ASSET_HEADERS),
            not_modified(),
            redirect(&["Location"]),
            not_modified().error(),
            err(404),
        ],
    },
    EndpointContract {
        name: "api version",
        method: "GET",
        head: true,
        path: "/API/VERSION",
        request_headers: &["If-None-Match"],
        summary_headers: SUMMARY_ASSET,
        outcomes: &[
            json(200, WireSchema::ApiVersion, ASSET_HEADERS),
            not_modified(),
            not_modified().error(),
        ],
    },
    EndpointContract {
        name: "site listing",
        method: "GET",
        head: true,
        path: "/FILES[/]",
        request_headers: &["Accept", "If-None-Match"],
        summary_headers: &[
            "Content-Type",
            "ETag",
            "Cache-Control",
            "Entry-Count",
            "Link",
        ],
        outcomes: &[
            json(200, WireSchema::DirectoryListing, ASSET_HEADERS).optional(PAGINATION_HEADERS),
            not_modified(),
            err(400),
            not_modified().error(),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "site redirect",
        method: "GET",
        head: true,
        path: "/{name}",
        request_headers: &[],
        summary_headers: &["Location"],
        outcomes: &[
            redirect(&["Location", "Content-Length"]),
            err(400),
            err(404),
        ],
    },
    EndpointContract {
        name: "site index",
        method: "GET",
        head: true,
        path: "/{name}/",
        request_headers: &["If-None-Match", "Range", "If-Range"],
        summary_headers: &[
            "Content-Type",
            "Content-Length",
            "ETag",
            "Cache-Control",
            "Expires",
        ],
        outcomes: &[
            binary(200, WireSchema::HostedContent, HOSTED_HEADERS),
            not_modified(),
            redirect(&["Location", "Content-Length"]),
            not_modified().error(),
            err(400),
            err(404),
            range_not_satisfiable(),
        ],
    },
    EndpointContract {
        name: "site put",
        method: "PUT",
        head: false,
        path: "/{name}[/]",
        request_headers: &[
            "Authorization",
            "Content-Type",
            "Content-Disposition",
            "Unpack",
            "Replace",
            "If-Match",
            "Idempotency-Key",
            "Creator-Claim",
            "Management-Action",
        ],
        summary_headers: SUMMARY_CREATE_MUTATION,
        outcomes: &[
            text(200, WireSchema::SiteMutationReceipt, RECEIPT_HEADERS)
                .forbid(FORBID_RANGE_REPLAYED),
            text(201, WireSchema::SiteMutationReceipt, RECEIPT_HEADERS)
                .forbid(FORBID_RANGE_REPLAYED),
            err(400),
            unauthorized(),
            err(403),
            err(404),
            err(409),
            precondition_failed(),
            err(413),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "site pop",
        method: "DELETE",
        head: false,
        path: "/{name}[/]",
        request_headers: &["Authorization"],
        summary_headers: SUMMARY_POP,
        outcomes: &[
            binary(200, WireSchema::Archive, ARCHIVE_UNDO_HEADERS)
                .forbid(FORBID_RANGE_ETAG_LOCATION),
            err(400),
            unauthorized(),
            err(404),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "site copy",
        method: "COPY",
        head: false,
        path: "/{name}[/]",
        request_headers: &[
            "Destination",
            "Idempotency-Key",
            "Creator-Claim",
            "Management-Action",
        ],
        summary_headers: SUMMARY_CREATE_MUTATION,
        outcomes: &[
            text(201, WireSchema::SiteMutationReceipt, RECEIPT_HEADERS).forbid(FORBID_RANGE),
            err(400),
            err(404),
            err(409),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "site move",
        method: "MOVE",
        head: false,
        path: "/{name}[/]",
        request_headers: &["Authorization", "Destination"],
        summary_headers: SUMMARY_MUTATION,
        outcomes: &[
            text(200, WireSchema::SiteMutationReceipt, RECEIPT_HEADERS)
                .forbid(FORBID_RANGE_REPLAYED),
            err(400),
            unauthorized(),
            err(404),
            err(409),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "site undo",
        method: "UNDO",
        head: false,
        path: "/{name}[/]",
        request_headers: &["Authorization", "Undo-Token"],
        summary_headers: SUMMARY_CONTENT_TYPE,
        outcomes: &[
            text(200, WireSchema::UndoMutationReceipt, CONTENT_TYPE),
            unauthorized(),
            err(404),
            err(409),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "site expire",
        method: "EXPIRE",
        head: false,
        path: "/{name}[/]",
        request_headers: &[
            "Authorization",
            "Expiry-Mode",
            "Expiry-In",
            "Expiry-At",
            "Expiry-Min-Age",
            "Expiry-Max-Age",
            "Expiry-Max-Size",
            "Expiry-Power",
        ],
        summary_headers: SUMMARY_EXPIRE,
        outcomes: &[
            json(200, WireSchema::ExpiryReport, UNDO_HEADERS).forbid(FORBID_RANGE_LOCATION_ETAG),
            err(400),
            unauthorized(),
            err(404),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "site management",
        method: "MANAGE",
        head: false,
        path: "/{name}[/]",
        request_headers: &[
            "Authorization",
            "Creator-Claim",
            "Management-Action",
            "Idempotency-Key",
        ],
        summary_headers: &[
            "Content-Type",
            "Management-Token",
            "Idempotency-Replayed",
            "Cache-Control",
        ],
        outcomes: &[
            json(200, WireSchema::ManagementStatus, CONTENT_TYPE_CACHE)
                .forbid(FORBID_RANGE_LOCATION_ETAG),
            err(400),
            unauthorized(),
            err(403),
            err(404),
            err(409),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "site file",
        method: "GET",
        head: true,
        path: "/{name}/{path...}",
        request_headers: &["Range", "If-Range", "If-None-Match"],
        summary_headers: SUMMARY_FILE,
        outcomes: &[
            binary(200, WireSchema::HostedContent, HOSTED_HEADERS),
            binary(206, WireSchema::HostedContent, PARTIAL_HEADERS).forbid(&["Location"]),
            not_modified(),
            redirect(&["Location", "Content-Length"]),
            not_modified().error(),
            err(400),
            err(404),
            range_not_satisfiable(),
        ],
    },
    EndpointContract {
        name: "file put",
        method: "PUT",
        head: false,
        path: "/{name}/{path...}",
        request_headers: &["Authorization", "If-Match", "Content-Type"],
        summary_headers: SUMMARY_MUTATION,
        outcomes: &[
            text(200, WireSchema::SiteMutationReceipt, RECEIPT_HEADERS)
                .forbid(FORBID_RANGE_REPLAYED),
            text(201, WireSchema::SiteMutationReceipt, RECEIPT_HEADERS)
                .forbid(FORBID_RANGE_REPLAYED),
            err(400),
            unauthorized(),
            err(403),
            err(404),
            err(409),
            precondition_failed(),
            err(413),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "file delete",
        method: "DELETE",
        head: false,
        path: "/{name}/{path...}",
        request_headers: &["Authorization"],
        summary_headers: &["Content-Type", "Undo-Token", "Undo-Expires"],
        outcomes: &[
            text(200, WireSchema::SiteMutationReceipt, UNDO_HEADERS)
                .forbid(FORBID_RANGE_LOCATION_ETAG),
            err(400),
            unauthorized(),
            err(404),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "file expire",
        method: "EXPIRE",
        head: false,
        path: "/{name}/{path...}",
        request_headers: &[
            "Authorization",
            "Expiry-Mode",
            "Expiry-In",
            "Expiry-At",
            "Expiry-Min-Age",
            "Expiry-Max-Age",
            "Expiry-Max-Size",
            "Expiry-Power",
        ],
        summary_headers: SUMMARY_EXPIRE,
        outcomes: &[
            json(200, WireSchema::ExpiryReport, UNDO_HEADERS).forbid(FORBID_RANGE_LOCATION_ETAG),
            err(400),
            unauthorized(),
            err(404),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "archive get",
        method: "GET",
        head: true,
        path: "/{name}.{tar|tar.gz|zip}",
        request_headers: &[],
        summary_headers: &[
            "Content-Type",
            "Content-Length",
            "Content-Disposition",
            "Cache-Control",
        ],
        outcomes: &[
            binary(
                200,
                WireSchema::Archive,
                &[
                    "Content-Type",
                    "Content-Length",
                    "Content-Disposition",
                    "Cache-Control",
                ],
            )
            .forbid(FORBID_RANGE_ETAG_LOCATION),
            err(400),
            err(404),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "archive pop",
        method: "DELETE",
        head: false,
        path: "/{name}.{tar|tar.gz|zip}",
        request_headers: &["Authorization"],
        summary_headers: SUMMARY_POP,
        outcomes: &[
            binary(200, WireSchema::Archive, ARCHIVE_UNDO_HEADERS)
                .forbid(FORBID_RANGE_ETAG_LOCATION),
            err(400),
            unauthorized(),
            err(404),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "files inventory",
        method: "GET",
        head: true,
        path: "/{name}/FILES[/]",
        request_headers: &["Accept", "If-None-Match"],
        summary_headers: &[
            "Content-Type",
            "ETag",
            "Content-Revision",
            "Cache-Control",
            "Entry-Count",
            "Link",
        ],
        outcomes: &[
            json(
                200,
                WireSchema::FileInventory,
                &["Content-Type", "ETag", "Content-Revision", "Cache-Control"],
            )
            .optional(PAGINATION_HEADERS),
            not_modified(),
            err(400),
            not_modified().error(),
            err(404),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "files subtree",
        method: "GET",
        head: true,
        path: "/{name}/FILES/{path...}",
        request_headers: &["Accept", "If-None-Match"],
        summary_headers: &[
            "Content-Type",
            "ETag",
            "Cache-Control",
            "Location",
            "Entry-Count",
            "Link",
        ],
        outcomes: &[
            json(200, WireSchema::DirectoryListing, ASSET_HEADERS).optional(PAGINATION_HEADERS),
            not_modified(),
            redirect(&["Location", "Content-Length"]),
            err(400),
            not_modified().error(),
            err(404),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "file hash",
        method: "GET",
        head: true,
        path: "/{name}/{path...}/HASH",
        request_headers: &[],
        summary_headers: SUMMARY_CONTENT_TYPE,
        outcomes: &[
            text(200, WireSchema::Hash, CONTENT_TYPE),
            err(400),
            err(404),
        ],
    },
    EndpointContract {
        name: "file raw",
        method: "GET",
        head: true,
        path: "/{name}/{path...}/RAW",
        request_headers: &["Range", "If-Range", "If-None-Match"],
        summary_headers: SUMMARY_FILE,
        outcomes: &[
            binary(200, WireSchema::HostedContent, HOSTED_HEADERS),
            binary(206, WireSchema::HostedContent, PARTIAL_HEADERS).forbid(&["Location"]),
            not_modified(),
            not_modified().error(),
            err(400),
            err(404),
            range_not_satisfiable(),
        ],
    },
    EndpointContract {
        name: "render asset",
        method: "GET",
        head: true,
        path: "/ASSETS/{bundle}/{path...}",
        request_headers: &["If-None-Match"],
        summary_headers: SUMMARY_ASSET,
        outcomes: &[
            binary(200, WireSchema::Asset, ASSET_HEADERS),
            not_modified(),
            not_modified().error(),
            err(404),
        ],
    },
    EndpointContract {
        name: "undo stack",
        method: "GET",
        head: true,
        path: "/{name}/UNDO[/]",
        request_headers: &[],
        summary_headers: SUMMARY_CONTENT_TYPE_CACHE,
        outcomes: &[
            json(200, WireSchema::UndoStack, CONTENT_TYPE_CACHE).forbid(FORBID_RANGE_LOCATION_ETAG),
            err(404),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "expiry inventory",
        method: "GET",
        head: true,
        path: "/{name}/EXPIRES[/]",
        request_headers: &[],
        summary_headers: SUMMARY_CONTENT_TYPE_CACHE,
        outcomes: &[
            json(200, WireSchema::ExpirySiteReport, CONTENT_TYPE_CACHE)
                .forbid(FORBID_RANGE_LOCATION_ETAG),
            err(404),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "expiry target",
        method: "GET",
        head: true,
        path: "/{name}/{path...}/EXPIRES",
        request_headers: &[],
        summary_headers: SUMMARY_CONTENT_TYPE_CACHE,
        outcomes: &[
            json(200, WireSchema::ExpiryReport, CONTENT_TYPE_CACHE)
                .forbid(FORBID_RANGE_LOCATION_ETAG),
            err(400),
            err(404),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "immutable blob",
        method: "GET",
        head: true,
        path: "/.blob/{name}/{hash}",
        request_headers: &["Range", "If-Range", "If-None-Match"],
        summary_headers: &[
            "Content-Type",
            "Content-Length",
            "Content-Range",
            "Accept-Ranges",
            "ETag",
            "Cache-Control",
        ],
        outcomes: &[
            binary(200, WireSchema::HostedContent, HOSTED_HEADERS),
            binary(206, WireSchema::HostedContent, PARTIAL_HEADERS).forbid(&["Location"]),
            not_modified(),
            not_modified().error(),
            err(400),
            err(404),
            range_not_satisfiable(),
        ],
    },
    EndpointContract {
        name: "alias batch",
        method: "ALIAS",
        head: false,
        path: "/{name}/",
        request_headers: &[
            "Alias-Target",
            "Authorization",
            "Content-Type",
            "Idempotency-Key",
            "If-Match",
        ],
        summary_headers: SUMMARY_RECEIPT,
        outcomes: &[
            json(200, WireSchema::AliasBatchReceipt, RECEIPT_HEADERS).forbid(FORBID_RANGE),
            json(201, WireSchema::AliasBatchReceipt, RECEIPT_HEADERS).forbid(FORBID_RANGE),
            err(400),
            unauthorized(),
            err(403),
            err(404),
            err(409),
            precondition_failed(),
            err(413),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "alias file",
        method: "ALIAS",
        head: false,
        path: "/{name}/{path...}",
        request_headers: &[
            "Alias-Target",
            "Authorization",
            "Idempotency-Key",
            "If-Match",
        ],
        summary_headers: SUMMARY_RECEIPT,
        outcomes: &[
            json(200, WireSchema::AliasReceipt, RECEIPT_HEADERS).forbid(FORBID_RANGE),
            json(201, WireSchema::AliasReceipt, RECEIPT_HEADERS).forbid(FORBID_RANGE),
            err(400),
            unauthorized(),
            err(403),
            err(404),
            err(409),
            precondition_failed(),
            err(413),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "allocated file",
        method: "POST",
        head: false,
        path: "/{name}/{folder...}/",
        request_headers: &[
            "Allocation-Action",
            "Allocation-Token",
            "Authorization",
            "Content-Type",
            "Expiry-At",
            "Expiry-In",
            "Expiry-Max-Age",
            "Expiry-Max-Size",
            "Expiry-Min-Age",
            "Expiry-Mode",
            "Expiry-Power",
            "File-Extension",
            "File-Name",
            "File-Prefix",
            "File-Suffix",
            "Idempotency-Key",
            "If-Match",
        ],
        summary_headers: &[
            "Content-Location",
            "Content-Revision",
            "Content-Type",
            "ETag",
            "Idempotency-Replayed",
            "Location",
            "Undo-Expires",
            "Undo-Token",
        ],
        outcomes: &[
            json(200, WireSchema::AllocationReceipt, ALLOCATION_HEADERS)
                .variant(RequestVariant::Create)
                .forbid(FORBID_RANGE),
            json(200, WireSchema::AllocationReceipt, ALLOCATION_HEADERS)
                .variant(RequestVariant::Finalize)
                .forbid(FORBID_RANGE),
            json(201, WireSchema::AllocationReceipt, ALLOCATION_HEADERS)
                .variant(RequestVariant::Create)
                .forbid(FORBID_RANGE),
            json(201, WireSchema::AllocationReceipt, ALLOCATION_HEADERS)
                .variant(RequestVariant::Finalize)
                .forbid(FORBID_RANGE),
            json(202, WireSchema::AllocationProposalReceipt, RECEIPT_HEADERS)
                .variant(RequestVariant::Propose)
                .forbid(FORBID_RANGE_CONTENT_LOCATION),
            json(
                200,
                WireSchema::AllocationCancellationReceipt,
                RECEIPT_HEADERS,
            )
            .variant(RequestVariant::Cancel)
            .forbid(FORBID_RANGE_CONTENT_LOCATION),
            err(400),
            unauthorized(),
            err(403),
            err(404),
            err(409),
            precondition_failed(),
            err(413),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "file replace",
        method: "REPLACE",
        head: false,
        path: "/{name}/{path...}",
        request_headers: &[
            "Authorization",
            "Content-Type",
            "Idempotency-Key",
            "If-Content-Match",
            "If-Match",
        ],
        summary_headers: SUMMARY_RECEIPT,
        outcomes: &[
            json(200, WireSchema::FileReplaceReceipt, RECEIPT_HEADERS).forbid(FORBID_RANGE),
            err(400),
            unauthorized(),
            err(403),
            err(404),
            err(409),
            precondition_failed(),
            err(413),
            err(500).retry_after(),
        ],
    },
    EndpointContract {
        name: "file splice",
        method: "PATCH",
        head: false,
        path: "/{name}/{path...}",
        request_headers: &[
            "Authorization",
            "Content-Type",
            "Idempotency-Key",
            "If-Content-Match",
            "If-Match",
            "Splice",
        ],
        summary_headers: SUMMARY_RECEIPT,
        outcomes: &[
            json(200, WireSchema::SpliceReceipt, RECEIPT_HEADERS).forbid(FORBID_RANGE),
            err(400),
            unauthorized(),
            err(403),
            err(404),
            err(409),
            precondition_failed(),
            err(413),
            err(416),
            err(500).retry_after(),
        ],
    },
];
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct ServingStats {
    pub cache: CacheStats,
    pub readers: ReaderStats,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub evictions: u64,
}

#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct ReaderStats {
    pub operations: u64,
    pub waits: u64,
    pub wait_micros: u64,
    pub query_micros: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SizeDistribution {
    pub min: Option<u64>,
    pub p25: Option<f64>,
    pub median: Option<f64>,
    pub mean: Option<f64>,
    pub p75: Option<f64>,
    pub max: Option<u64>,
    pub iqr: Option<f64>,
    pub stddev: Option<f64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Stats {
    pub sites: u64,
    pub files: u64,
    pub aliases: u64,
    pub blobs: u64,
    pub bytes: u64,
    pub logical_bytes: u64,
    pub saved_bytes: u64,
    pub saved_fraction: f64,
    pub file_sizes: SizeDistribution,
    pub blob_sizes: SizeDistribution,
    pub serving: ServingStats,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ManagementStatus {
    pub managed: bool,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct InventoryFile {
    pub path: String,
    pub hash: String,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AliasTargetKind {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct InventoryAlias {
    pub path: String,
    pub target: String,
    pub target_kind: Option<AliasTargetKind>,
    pub dangling: bool,
    pub resolved_hash: Option<String>,
    pub size: Option<u64>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SiteInventory {
    pub site: String,
    pub created_at: Option<String>,
    pub updated_at: String,
    pub content_revision: u64,
    pub tree_hash: String,
    // `files` and `aliases` precede `events` on purpose. Shell clients before
    // 1.0 scan this JSON for the first `"files"` key, and each event carries
    // a `files` count of its own; with events first they found that count
    // instead of the array and listed nothing.
    pub files: Vec<InventoryFile>,
    pub aliases: Vec<InventoryAlias>,
    pub events: Vec<SiteEvent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum SiteEventKind {
    Created,
    Publish,
    Rename,
    Restore,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct SiteEvent {
    pub kind: SiteEventKind,
    pub at: String,
    pub files: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UndoEntry {
    pub token: String,
    pub kind: String,
    pub description: String,
    pub created_at: String,
    pub expires_at: String,
    pub remaining_seconds: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct UndoStack {
    pub site: String,
    pub entries: Vec<UndoEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ExpiryMode {
    Relative,
    Absolute,
    Decay,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ExpiryTargetKind {
    Site,
    Folder,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct ExpiryTarget {
    pub site: String,
    pub path: Option<String>,
    pub kind: ExpiryTargetKind,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct OwnExpiryReport {
    pub mode: ExpiryMode,
    pub min_age_seconds: Option<u64>,
    pub max_age_seconds: Option<u64>,
    pub max_size_bytes: Option<u64>,
    pub power: Option<f64>,
    pub retention_seconds: Option<u64>,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct InheritedExpiryCap {
    pub kind: ExpiryTargetKind,
    pub path: Option<String>,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub struct ExpiryLimit {
    pub kind: ExpiryTargetKind,
    pub path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ExpiryReport {
    pub target: ExpiryTarget,
    pub size: u64,
    pub refreshed_at: Option<String>,
    pub own_policy: Option<OwnExpiryReport>,
    pub inherited_caps: Vec<InheritedExpiryCap>,
    pub effective_expires_at: Option<String>,
    pub remaining_seconds: Option<u64>,
    pub limited_by: Option<ExpiryLimit>,
}

#[derive(Debug, Clone, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ExpirySiteReport {
    pub site: String,
    pub entries: Vec<ExpiryReport>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ListingKind {
    Builtin,
    Site,
    Directory,
    File,
    Alias,
}

#[derive(Debug, Clone)]
pub struct ListingEntry {
    pub kind: ListingKind,
    pub name: String,
    pub files: Option<u64>,
    pub bytes: u64,
    pub target: Option<String>,
    pub target_kind: Option<AliasTargetKind>,
    pub dangling: Option<bool>,
}

impl serde::Serialize for ListingEntry {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        use serde::ser::SerializeStruct as _;

        match self.kind {
            ListingKind::Builtin | ListingKind::Site | ListingKind::Directory => {
                let mut entry = serializer.serialize_struct("ListingEntry", 4)?;
                entry.serialize_field("kind", &self.kind)?;
                entry.serialize_field("name", &self.name)?;
                entry.serialize_field("files", &self.files)?;
                entry.serialize_field("bytes", &self.bytes)?;
                entry.end()
            }
            ListingKind::File => {
                let mut entry = serializer.serialize_struct("ListingEntry", 3)?;
                entry.serialize_field("kind", &self.kind)?;
                entry.serialize_field("name", &self.name)?;
                entry.serialize_field("bytes", &self.bytes)?;
                entry.end()
            }
            ListingKind::Alias => {
                let dangling = self.dangling.unwrap_or(true);
                let bytes = (!dangling).then_some(self.bytes);
                let mut entry = serializer.serialize_struct("ListingEntry", 7)?;
                entry.serialize_field("kind", &self.kind)?;
                entry.serialize_field("name", &self.name)?;
                entry.serialize_field("target", &self.target)?;
                entry.serialize_field("target_kind", &self.target_kind)?;
                entry.serialize_field("dangling", &dangling)?;
                entry.serialize_field("files", &self.files)?;
                entry.serialize_field("bytes", &bytes)?;
                entry.end()
            }
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Listing {
    pub path: String,
    pub files: u64,
    pub aliases: u64,
    pub bytes: u64,
    pub entries: Vec<ListingEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct AliasDefinition {
    pub path: String,
    pub target: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct AliasBatchRequest {
    pub aliases: Vec<AliasDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UndoReceipt {
    pub token: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct MutationReceipt {
    pub changed: bool,
    pub replayed: bool,
    pub idempotency_key: String,
    pub location: String,
    pub etag: String,
    pub content_revision: u64,
    pub sanitized_management_tokens: u64,
    pub sanitized_creator_claims: u64,
    pub undo: Option<UndoReceipt>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AliasReceipt {
    pub path: String,
    pub target: String,
    pub target_kind: Option<AliasTargetKind>,
    pub dangling: bool,
    pub resolved_hash: Option<String>,
    pub size: Option<u64>,
    #[serde(flatten)]
    pub mutation: MutationReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AliasBatchReceipt {
    pub aliases: Vec<InventoryAlias>,
    #[serde(flatten)]
    pub mutation: MutationReceipt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AllocationOutcome {
    Created,
    Existing,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum AllocationNaming {
    Generated {
        prefix: String,
        extension: String,
        suffix: String,
    },
    Custom,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AllocatedFileReceipt {
    pub outcome: AllocationOutcome,
    pub site: String,
    pub path: String,
    pub name: String,
    pub url: String,
    pub hash: String,
    pub size: u64,
    pub blob_url: String,
    pub naming: AllocationNaming,
    #[serde(flatten)]
    pub mutation: MutationReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ProposedFileName {
    pub folder: String,
    pub default_name: String,
    pub hash: String,
    pub size: u64,
    pub media_type: String,
    pub inferred_extension: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AllocationProposalReceipt {
    pub allocation_token: String,
    pub expires_at: String,
    pub proposal: ProposedFileName,
    pub idempotency_key: String,
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct AllocationCancellationReceipt {
    pub allocation_token: String,
    pub cancelled: bool,
    pub idempotency_key: String,
    pub replayed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ReplacementOutcome {
    Replaced,
    Relocated,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FileReplaceReceipt {
    pub outcome: ReplacementOutcome,
    pub old_path: String,
    pub new_path: String,
    pub relocated: bool,
    pub old_hash: String,
    pub new_hash: String,
    pub size: u64,
    #[serde(flatten)]
    pub mutation: MutationReceipt,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct SpliceReceipt {
    pub old_path: String,
    pub new_path: String,
    pub relocated: bool,
    pub old_hash: String,
    pub new_hash: String,
    pub old_size: u64,
    pub new_size: u64,
    pub splices: usize,
    #[serde(flatten)]
    pub mutation: MutationReceipt,
}

pub const CONTRACT_FIXTURE_VERSION: u32 = 2;
pub const INITIAL_API_VERSION: ContractVersion = ContractVersion::new(0, 1, 0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ExtensionNormalizationVector {
    pub input: &'static str,
    pub output: Option<&'static str>,
}

pub const EXTENSION_NORMALIZATION_VECTORS: &[ExtensionNormalizationVector] = &[
    ExtensionNormalizationVector {
        input: ".Tar.GZ",
        output: Some("tar-gz"),
    },
    ExtensionNormalizationVector {
        input: "hello_world",
        output: Some("hello-world"),
    },
    ExtensionNormalizationVector {
        input: "a--b",
        output: Some("a-b"),
    },
    ExtensionNormalizationVector {
        input: "../png",
        output: Some("png"),
    },
    ExtensionNormalizationVector {
        input: "café",
        output: Some("caf"),
    },
    ExtensionNormalizationVector {
        input: "日本",
        output: None,
    },
    ExtensionNormalizationVector {
        input: "...",
        output: None,
    },
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContractVersion {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl ContractVersion {
    #[must_use]
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl std::fmt::Display for ContractVersion {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl serde::Serialize for ContractVersion {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.collect_str(self)
    }
}

#[derive(Debug, serde::Serialize)]
pub struct ContractFixture {
    pub fixture_version: u32,
    pub extension_normalization: &'static [ExtensionNormalizationVector],
    pub operations: Vec<OperationFixture>,
}

#[derive(Debug, serde::Serialize)]
pub struct OperationFixture {
    pub name: &'static str,
    pub method: &'static str,
    pub head: bool,
    pub path: &'static str,
    pub introduced: ContractVersion,
    pub request_headers: &'static [&'static str],
    pub response_headers: Vec<&'static str>,
    pub outcomes_exact: bool,
    pub success_outcomes: Vec<FixtureOutcome>,
    pub error_outcomes: Vec<FixtureOutcome>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct FixtureOutcome {
    pub status: u16,
    pub request_variant: &'static str,
    pub body: WireBody,
    pub schema: WireSchema,
    pub required_headers: &'static [&'static str],
    pub optional_headers: &'static [&'static str],
    pub forbidden_headers: &'static [&'static str],
}

#[must_use]
pub fn contract_fixture() -> ContractFixture {
    ContractFixture {
        fixture_version: CONTRACT_FIXTURE_VERSION,
        extension_normalization: EXTENSION_NORMALIZATION_VECTORS,
        operations: ENDPOINTS.iter().map(operation_fixture).collect(),
    }
}

#[must_use]
/// Serializes the deterministic contract fixture.
///
/// # Panics
///
/// Panics only if serialization of the statically typed fixture fails.
pub fn contract_fixture_json() -> String {
    let mut json = serde_json::to_string_pretty(&contract_fixture())
        .expect("contract fixture is serializable");
    json.push('\n');
    json
}

fn operation_fixture(endpoint: &'static EndpointContract) -> OperationFixture {
    let mut success_outcomes = Vec::new();
    let mut error_outcomes = Vec::new();
    for outcome in endpoint.outcomes {
        let fixture = FixtureOutcome {
            status: outcome.status,
            request_variant: outcome.request_variant.as_str(),
            body: outcome.body,
            schema: outcome.schema,
            required_headers: outcome.required_headers,
            optional_headers: outcome.optional_headers,
            forbidden_headers: outcome.forbidden_headers,
        };
        match outcome.kind {
            OutcomeKind::Success => success_outcomes.push(fixture),
            OutcomeKind::Error => error_outcomes.push(fixture),
        }
    }
    OperationFixture {
        name: endpoint.name,
        method: endpoint.method,
        head: endpoint.head,
        path: endpoint.path,
        introduced: INITIAL_API_VERSION,
        request_headers: endpoint.request_headers,
        response_headers: endpoint.response_headers(),
        outcomes_exact: true,
        success_outcomes,
        error_outcomes,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{
        ENDPOINTS, INITIAL_API_VERSION, OutcomeKind, RequestVariant, WireSchema, contract_fixture,
    };

    #[test]
    fn endpoint_outcomes_are_owned_exact_and_well_formed() {
        let mut names = HashSet::new();
        for endpoint in ENDPOINTS {
            assert!(names.insert(endpoint.name), "duplicate {}", endpoint.name);
            assert!(!endpoint.outcomes.is_empty(), "{} outcomes", endpoint.name);
            assert!(
                endpoint
                    .outcomes
                    .iter()
                    .any(|outcome| outcome.kind == OutcomeKind::Success)
            );
            for outcome in endpoint.outcomes {
                assert!((100..=599).contains(&outcome.status));
                assert!(!outcome.request_variant.as_str().is_empty());
                assert!(
                    outcome
                        .required_headers
                        .iter()
                        .all(|header| !outcome.forbidden_headers.contains(header))
                );
                assert!(
                    outcome
                        .optional_headers
                        .iter()
                        .all(|header| !outcome.forbidden_headers.contains(header))
                );
            }
        }
    }

    #[test]
    fn allocation_variants_are_explicit_exact_records() {
        let allocated = ENDPOINTS
            .iter()
            .find(|endpoint| endpoint.name == "allocated file")
            .unwrap();
        for (status, variant, schema) in [
            (200, RequestVariant::Create, WireSchema::AllocationReceipt),
            (200, RequestVariant::Finalize, WireSchema::AllocationReceipt),
            (201, RequestVariant::Create, WireSchema::AllocationReceipt),
            (201, RequestVariant::Finalize, WireSchema::AllocationReceipt),
            (
                202,
                RequestVariant::Propose,
                WireSchema::AllocationProposalReceipt,
            ),
            (
                200,
                RequestVariant::Cancel,
                WireSchema::AllocationCancellationReceipt,
            ),
        ] {
            assert_eq!(
                allocated
                    .exact_outcome(OutcomeKind::Success, status, variant)
                    .unwrap()
                    .schema,
                schema
            );
        }
        assert!(
            allocated
                .exact_outcome(OutcomeKind::Success, 202, RequestVariant::Default)
                .is_none()
        );
    }

    #[test]
    fn response_header_summaries_are_derived_from_outcomes() {
        let fixture = contract_fixture();
        for (operation, endpoint) in fixture.operations.iter().zip(ENDPOINTS) {
            assert_eq!(
                operation.response_headers,
                endpoint.response_headers(),
                "{}",
                endpoint.name
            );
        }
    }

    #[test]
    fn missing_exact_outcome_queries_are_explicit() {
        let docs = ENDPOINTS
            .iter()
            .find(|endpoint| endpoint.name == "docs")
            .unwrap();
        assert!(
            docs.exact_outcome(OutcomeKind::Success, 599, RequestVariant::Default)
                .is_none()
        );
        assert!(
            docs.exact_outcome(OutcomeKind::Error, 200, RequestVariant::Default)
                .is_none()
        );
    }

    #[test]
    fn fixture_is_deterministic_and_covers_every_operation() {
        let fixture = contract_fixture();
        assert_eq!(fixture.fixture_version, 2);
        assert_eq!(fixture.operations.len(), ENDPOINTS.len());
        for (operation, endpoint) in fixture.operations.iter().zip(ENDPOINTS) {
            assert_eq!(operation.name, endpoint.name);
            assert_eq!(operation.introduced, INITIAL_API_VERSION);
            assert!(operation.outcomes_exact, "{} outcomes", operation.name);
            assert_eq!(
                operation.success_outcomes.len() + operation.error_outcomes.len(),
                endpoint.outcomes.len()
            );
        }
        let first = super::contract_fixture_json();
        let second = super::contract_fixture_json();
        assert_eq!(first, second);
        assert!(first.ends_with('\n'));
        serde_json::from_str::<serde_json::Value>(&first).expect("fixture is valid JSON");
    }
}
