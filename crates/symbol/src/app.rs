//! Command line, shared server state, creator identity and background work.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderName, HeaderValue, Request};
use axum::middleware::Next;
use axum::response::Response;
use clap::{Parser, Subcommand};
use tokio::sync::Semaphore;

use crate::docs::{API_REVISION, API_SOURCE_HASH, API_VERSION};
use crate::page;
use crate::store::{Store, StoreError};

pub const DEFAULT_MAX_ARCHIVE_UPLOAD: u64 = 50 * 1024 * 1024;
const DEFAULT_MAX_ARCHIVE_EXTRACTED: u64 = 80 * 1024 * 1024;
const DEFAULT_MAX_ARCHIVE_FILES: usize = 5000;
pub const DEFAULT_MAX_FILE_SIZE: u64 = 4 * 1024 * 1024 * 1024;

#[derive(Parser)]
#[command(name = "symbol", about = "Tiny static-site hosting for the tailnet")]
pub struct Args {
    #[arg(long, default_value = "127.0.0.1:4340", env = "SYMBOL_BIND")]
    pub bind: String,
    #[arg(long, default_value = "/var/lib/symbol", env = "SYMBOL_ROOT")]
    pub root: PathBuf,
    #[arg(
        long,
        default_value_t = DEFAULT_MAX_FILE_SIZE,
        env = "SYMBOL_MAX_FILE_SIZE"
    )]
    pub max_file_size: u64,
    #[arg(
        long,
        default_value_t = DEFAULT_MAX_ARCHIVE_UPLOAD,
        env = "SYMBOL_MAX_ARCHIVE_UPLOAD"
    )]
    pub max_archive_upload: u64,
    #[arg(
        long,
        default_value_t = DEFAULT_MAX_ARCHIVE_EXTRACTED,
        env = "SYMBOL_MAX_ARCHIVE_EXTRACTED"
    )]
    pub max_archive_extracted: u64,
    #[arg(
        long,
        default_value_t = DEFAULT_MAX_ARCHIVE_FILES,
        env = "SYMBOL_MAX_ARCHIVE_FILES"
    )]
    pub max_archive_files: usize,
    #[arg(long, env = "SYMBOL_PUBLIC_URL")]
    pub public_url: Option<String>,
    #[arg(long, env = "SYMBOL_ALLOW_DEV_ORIGIN", default_value_t = false)]
    pub allow_dev_origin: bool,
    #[arg(long, env = "SYMBOL_EXPIRY_MIN_AGE", default_value = "30d")]
    pub expiry_min_age: String,
    #[arg(long, env = "SYMBOL_EXPIRY_MAX_AGE", default_value = "365d")]
    pub expiry_max_age: String,
    #[arg(long, env = "SYMBOL_EXPIRY_MAX_SIZE", default_value = "512MiB")]
    pub expiry_max_size: String,
    #[arg(long, env = "SYMBOL_EXPIRY_POWER", default_value_t = 3.0)]
    pub expiry_power: f64,
    #[arg(long, env = "SYMBOL_TRUSTED_PROXY_PRINCIPAL_HEADER")]
    pub trusted_proxy_principal_header: Option<HeaderName>,
    #[arg(long, env = "SYMBOL_MTLS_PRINCIPAL_HEADER")]
    pub mtls_principal_header: Option<HeaderName>,
    #[arg(long, env = "SYMBOL_TAILSCALE_USER_HEADER")]
    pub tailscale_user_header: Option<HeaderName>,
    #[arg(long, env = "SYMBOL_TAILSCALE_WHOIS_COMMAND")]
    pub tailscale_whois_command: Option<PathBuf>,
    #[arg(long, env = "SYMBOL_TRUSTED_PROXY", value_delimiter = ',')]
    pub trusted_proxy: Vec<IpAddr>,
    #[arg(long, env = "SYMBOL_AUDIT_TRUSTED_PROXY", value_delimiter = ',')]
    pub audit_trusted_proxy: Vec<IpAddr>,
    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    Contract,
    Admin {
        #[command(subcommand)]
        action: AdminAction,
    },
}

#[derive(Subcommand)]
pub enum AdminAction {
    Claim {
        name: String,
    },
    Rotate {
        name: String,
        #[arg(long)]
        token: String,
    },
}

#[derive(Clone)]
pub struct App {
    pub store: Store,
    pub store_tasks: Arc<Semaphore>,
    pub hashes: Arc<Mutex<HashMap<String, String>>>,
    pub max_file_size: u64,
    pub max_archive_upload: u64,
    pub public_url: Arc<str>,
    /// Special pages with `${host}` already resolved for this deployment.
    pub pages: Arc<page::Rendered>,
    pub identity_provider: IdentityProvider,
    pub audit_trusted_proxy: Arc<[IpAddr]>,
}

#[derive(Clone)]
pub enum IdentityProvider {
    Receipt,
    TrustedProxy {
        principal_header: HeaderName,
        peers: Arc<[IpAddr]>,
    },
    Mtls {
        principal_header: HeaderName,
        peers: Arc<[IpAddr]>,
    },
    Tailscale {
        principal_header: HeaderName,
        peers: Arc<[IpAddr]>,
    },
    TailscaleLocal {
        command: Arc<FsPath>,
    },
}

pub const INTERNAL_CREATOR_HEADER: &str = "x-symbol-internal-creator-principal";

#[derive(Clone, Copy)]
pub struct AuditIp(pub Option<IpAddr>);

#[derive(Clone)]
pub struct RequestIdentityConfig {
    pub provider: IdentityProvider,
    pub audit_trusted_proxy: Arc<[IpAddr]>,
}

impl App {
    #[cfg(test)]
    pub fn new(store: Store) -> Self {
        Self::with_options(
            store,
            DEFAULT_MAX_FILE_SIZE,
            DEFAULT_MAX_ARCHIVE_UPLOAD,
            "http://symbol".into(),
            IdentityProvider::Receipt,
        )
    }

    #[cfg(test)]
    pub fn with_max_file_size(store: Store, max_file_size: u64) -> Self {
        Self::with_options(
            store,
            max_file_size,
            DEFAULT_MAX_ARCHIVE_UPLOAD,
            "http://symbol".into(),
            IdentityProvider::Receipt,
        )
    }

    pub fn with_options(
        store: Store,
        max_file_size: u64,
        max_archive_upload: u64,
        public_url: String,
        identity_provider: IdentityProvider,
    ) -> Self {
        Self {
            store_tasks: Arc::new(Semaphore::new(store.blocking_capacity())),
            store,
            hashes: Arc::new(Mutex::new(HashMap::new())),
            max_file_size,
            max_archive_upload,
            pages: Arc::new(page::Rendered::new(&public_url)),
            public_url: public_url.into(),
            identity_provider,
            audit_trusted_proxy: Arc::from([]),
        }
    }

    pub async fn run_store<T, F>(&self, work: F) -> Result<T, StoreError>
    where
        T: Send + 'static,
        F: FnOnce(Store) -> Result<T, StoreError> + Send + 'static,
    {
        let permit = Arc::clone(&self.store_tasks)
            .acquire_owned()
            .await
            .expect("store semaphore stays open");
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work(store)
        })
        .await
        .expect("store task panicked")
    }
}

pub fn configured_identity_provider(args: &mut Args) -> IdentityProvider {
    let mut configured = [
        (1_u8, args.trusted_proxy_principal_header.take()),
        (2_u8, args.mtls_principal_header.take()),
        (3_u8, args.tailscale_user_header.take()),
    ]
    .into_iter()
    .filter_map(|(kind, header)| header.map(|header| (kind, header)))
    .collect::<Vec<_>>();
    assert!(
        configured.len() <= 1,
        "configure only one creator identity provider"
    );
    if let Some(command) = args.tailscale_whois_command.take() {
        assert!(
            configured.is_empty(),
            "Tailscale LocalAPI and identity headers are mutually exclusive"
        );
        return IdentityProvider::TailscaleLocal {
            command: Arc::from(command),
        };
    }
    let peers: Arc<[IpAddr]> = std::mem::take(&mut args.trusted_proxy).into();
    // `configured` holds at most one entry, as asserted above.
    match (configured.pop(), peers.is_empty()) {
        (None, true) => IdentityProvider::Receipt,
        (Some((1, principal_header)), false) => IdentityProvider::TrustedProxy {
            principal_header,
            peers,
        },
        (Some((2, principal_header)), false) => IdentityProvider::Mtls {
            principal_header,
            peers,
        },
        (Some((3, principal_header)), false) => IdentityProvider::Tailscale {
            principal_header,
            peers,
        },
        _ => {
            panic!("identity principal header and SYMBOL_TRUSTED_PROXY must be configured together")
        }
    }
}

pub async fn attach_api_identity(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert("symbol-api-version", HeaderValue::from_static(API_VERSION));
    headers.insert(
        "symbol-api-revision",
        HeaderValue::from_static(API_REVISION),
    );
    headers.insert(
        "symbol-api-source-hash",
        HeaderValue::from_static(API_SOURCE_HASH),
    );
    response
}

pub async fn resolve_creator(
    State(config): State<RequestIdentityConfig>,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    request.headers_mut().remove(INTERNAL_CREATOR_HEADER);
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|peer| peer.0.ip());
    let audit_ip = peer.and_then(|direct| {
        if config.audit_trusted_proxy.contains(&direct) {
            request
                .headers()
                .get("x-forwarded-for")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.split(',').next())
                .and_then(|value| value.trim().parse::<IpAddr>().ok())
                .or(Some(direct))
        } else {
            Some(direct)
        }
    });
    request.extensions_mut().insert(AuditIp(audit_ip));
    if let IdentityProvider::TailscaleLocal { command } = &config.provider
        && let Some(ip) = audit_ip
        && let Some(principal) = tailscale_principal(command, ip).await
        && let Ok(internal) = HeaderValue::from_str(&format!("tailscale:{principal}"))
    {
        request
            .headers_mut()
            .insert(INTERNAL_CREATOR_HEADER, internal);
    }
    let configured = match &config.provider {
        IdentityProvider::Receipt | IdentityProvider::TailscaleLocal { .. } => None,
        IdentityProvider::TrustedProxy {
            principal_header,
            peers,
        } => Some(("proxy", principal_header, peers)),
        IdentityProvider::Mtls {
            principal_header,
            peers,
        } => Some(("mtls", principal_header, peers)),
        IdentityProvider::Tailscale {
            principal_header,
            peers,
        } => Some(("tailscale", principal_header, peers)),
    };
    if let Some((kind, principal_header, peers)) = configured
        && let Some(peer) = peer
        && peers.contains(&peer)
        && let Some(principal) = request
            .headers()
            .get(principal_header)
            .and_then(|value| value.to_str().ok())
        && let Ok(internal) = HeaderValue::from_str(&format!("{kind}:{principal}"))
    {
        request
            .headers_mut()
            .insert(INTERNAL_CREATOR_HEADER, internal);
    }
    next.run(request).await
}

async fn tailscale_principal(command: &FsPath, ip: IpAddr) -> Option<String> {
    let output = tokio::process::Command::new(command)
        .args(["whois", "--json", &ip.to_string()])
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
    value
        .pointer("/UserProfile/LoginName")
        .and_then(serde_json::Value::as_str)
        .filter(|principal| !principal.is_empty())
        .map(str::to_string)
}

pub async fn shutdown() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("sigterm");
        tokio::select! {
            _ = ctrl_c => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    ctrl_c.await.ok();
}

pub fn validate_public_url(url: &str) -> Result<(), &'static str> {
    if !(url.starts_with("http://") || url.starts_with("https://"))
        || url.ends_with('/')
        || url
            .split_once("://")
            .is_none_or(|(_, rest)| rest.is_empty() || rest.contains('/'))
    {
        return Err("must be an absolute http(s) origin without a trailing slash or path");
    }
    Ok(())
}

pub async fn expiry_worker(app: App) {
    loop {
        expiry_worker_iteration(&app).await;
    }
}

#[expect(clippy::cognitive_complexity)]
async fn expiry_worker_iteration(app: &App) {
    let delay = match app.run_store(|store| store.next_expiry_delay()).await {
        Ok(delay) => delay,
        Err(err) => {
            tracing::warn!(%err, "failed to determine next expiry deadline");
            std::time::Duration::from_secs(60)
        }
    };
    tokio::time::sleep(delay).await;
    match app.run_store(|store| store.sweep_expired()).await {
        Ok(0) => {}
        Ok(expired) => tracing::info!(expired, "expired hosted targets"),
        Err(err) => tracing::warn!(%err, "expiry sweep failed"),
    }
}
