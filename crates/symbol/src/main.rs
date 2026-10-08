macro_rules! static_asset {
    ($name:literal) => {
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../static/", $name))
    };
}

macro_rules! generated_asset {
    ($name:literal) => {
        include_str!(concat!(env!("OUT_DIR"), "/", $name))
    };
}

mod api_error;
mod app;
mod assets;
mod blob;
mod blob_store;
mod browse;
#[cfg(test)]
mod contract_conformance;
mod database;
mod docs;
mod expiry;
mod hash;
mod headers;
mod html_charset;
mod http_cache;
mod lifecycle;
mod markdown;
mod markdown_cache;
mod mutation_http;
mod name;
mod numeric;
mod page;
mod pathutil;
mod publish;
mod response;
mod routes;
mod sanitize;
mod schema;
mod secrets;
mod serve;
mod splice;
mod store;
#[cfg(test)]
mod tests;
mod units;
mod upload;

use std::net::SocketAddr;

use clap::Parser;
use expiry::DecayPolicy;
use secrets::ManagementToken;
use store::Store;
use symbol_contract as contract;
use tokio::net::TcpListener;

use app::{
    AdminAction, App, Args, Command, configured_identity_provider, expiry_worker, shutdown,
    validate_public_url,
};
use routes::{log_mutation_signals_ready, router};

// Items other modules reach as `crate::name`.
pub(crate) use headers::{if_match_from, insert_sanitized_headers, insert_undo_headers};
pub(crate) use lifecycle::{ExpiryRequest, expiry_policy_from, has_expiry_parameters};
pub(crate) use publish::{TemporaryUpload, authorize};
pub(crate) use response::{created_or_ok, plain};

// The test modules reach everything through `use super::*`.
#[cfg(test)]
use axum::{
    Router,
    http::{StatusCode, header},
    response::Response,
};
#[cfg(test)]
use hash::ContentHash;
#[cfg(test)]
use store::StoreError;
#[cfg(test)]
pub(crate) use {app::*, blob::*, docs::*, routes::*, serve::*};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mut args = Args::parse();
    if matches!(args.command.as_ref(), Some(Command::Contract)) {
        println!(
            "{}",
            serde_json::to_string_pretty(contract::ENDPOINTS).expect("contract is serializable")
        );
        return;
    }
    let is_admin = args.command.is_some();
    let mut public_url = args.public_url.take().unwrap_or_else(|| {
        assert!(
            args.allow_dev_origin || is_admin,
            "SYMBOL_PUBLIC_URL is required (or set SYMBOL_ALLOW_DEV_ORIGIN=true for development)"
        );
        "http://symbol".to_string()
    });
    validate_public_url(&public_url).expect("valid SYMBOL_PUBLIC_URL");
    let listener = if is_admin {
        None
    } else {
        let listener = TcpListener::bind(&args.bind)
            .await
            .unwrap_or_else(|err| panic!("bind {}: {err}", args.bind));
        let bound = listener.local_addr().expect("bound listener address");
        if let Some(prefix) = public_url.strip_suffix(":0") {
            public_url = format!("{prefix}:{}", bound.port());
        }
        Some(listener)
    };
    let expiry_defaults = DecayPolicy {
        min_age_seconds: expiry::parse_duration_seconds(&args.expiry_min_age)
            .expect("valid SYMBOL_EXPIRY_MIN_AGE"),
        max_age_seconds: expiry::parse_duration_seconds(&args.expiry_max_age)
            .expect("valid SYMBOL_EXPIRY_MAX_AGE"),
        max_size_bytes: expiry::parse_size_bytes(&args.expiry_max_size)
            .expect("valid SYMBOL_EXPIRY_MAX_SIZE"),
        power: args.expiry_power,
    };
    upload::configure_archive_limits(upload::ArchiveLimits {
        max_files: args.max_archive_files,
        max_extracted: args.max_archive_extracted,
    })
    .expect("archive limits are configured once");
    let root = std::mem::take(&mut args.root);
    let store = Store::with_expiry_defaults(root, public_url.clone(), expiry_defaults)
        .expect("create data directory");
    if let Some(Command::Admin { action }) = args.command.take() {
        let (name, token, verb) = match action {
            AdminAction::Claim { name } => {
                let token = store
                    .operator_claim(&name)
                    .expect("operator management claim");
                (name, token, "claimed")
            }
            AdminAction::Rotate { name, token } => {
                let current =
                    ManagementToken::parse(&token).expect("valid current management token");
                let replacement = store
                    .operator_rotate(&name, &current)
                    .expect("operator management rotation");
                (name, replacement, "rotated")
            }
        };
        println!("operator-{verb} {name}");
        println!("management token (shown once):");
        println!("  {}", token.encode());
        return;
    }
    let identity_provider = configured_identity_provider(&mut args);
    let mut state = App::with_options(
        store,
        args.max_file_size,
        args.max_archive_upload,
        public_url,
        identity_provider,
    );
    state.audit_trusted_proxy = std::mem::take(&mut args.audit_trusted_proxy).into();
    tokio::spawn(expiry_worker(state.clone()));
    let app = router(state);
    let listener = listener.expect("server mode binds a listener");
    let bound = listener.local_addr().expect("bound listener address");
    log_mutation_signals_ready();
    tracing::info!("listening on {bound}");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown())
    .await
    .expect("server");
}
