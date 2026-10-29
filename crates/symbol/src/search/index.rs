//! The content indexer: one background task that keeps `search.sqlite` in
//! step with the live files.
//!
//! It wakes after every successful mutation, at startup, and every minute as
//! a safety net for expiry sweeps and missed signals. Each pass asks the
//! store for blobs the index has not looked at, reads and extracts them
//! outside any transaction, and records a batch at a time, so a backfill of
//! a large site never holds a lock for long. Garbage collection, which only
//! reclaims the space of documents nothing references, runs hourly.
//!
//! The index is keyed by blob, and queries join the live files, so nothing
//! here runs on delete, move or replace: those are visible at once. The one
//! lag is between publishing new content and this task indexing it, which
//! [`Indexer::pending`] reports.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::middleware::Next;
use axum::response::Response;
use tokio::sync::Notify;

use super::extract::{self, Document, MAX_BLOB_BYTES};
use crate::app::App;
use crate::hash::ContentHash;
use crate::routes::is_mutation_method;
use crate::store::{Store, StoreError, Unindexed};

/// Blobs read and recorded per transaction.
const BATCH: usize = 256;
/// How long the task sleeps without a wake-up before looking anyway.
const TICK: Duration = Duration::from_mins(1);
const GC_EVERY: Duration = Duration::from_hours(1);

/// What the handler and the middleware share with the task.
#[derive(Default)]
pub struct Indexer {
    wake: Notify,
    /// Live blobs the index has not looked at, as of the last pass.
    pending: AtomicU64,
}

impl Indexer {
    /// Asks the task to look for new content now.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// How many live blobs are not yet searchable by content.
    pub fn pending(&self) -> u64 {
        self.pending.load(Ordering::Relaxed)
    }
}

/// Wakes the indexer after a mutation that succeeded.
pub async fn wake_after_mutation(
    State(indexer): State<Arc<Indexer>>,
    request: Request<Body>,
    next: Next,
) -> Response {
    let mutation = is_mutation_method(request.method());
    let response = next.run(request).await;
    if mutation && response.status().is_success() {
        indexer.wake();
    }
    response
}

/// The background task; see the module documentation.
pub async fn worker(app: App) {
    let mut last_collection = Instant::now();
    let mut backfilled = false;
    loop {
        backfilled = run_pass(&app, backfilled).await;
        if last_collection.elapsed() >= GC_EVERY {
            last_collection = Instant::now();
            collect_garbage(&app).await;
        }
        wait_for_work(&app).await;
    }
}

/// One pass over everything pending. The first pass that succeeds is the
/// startup backfill, which ends with an optimize and a status line; the
/// return value says whether that has happened.
async fn run_pass(app: &App, backfilled: bool) -> bool {
    match index_pending(app).await {
        Ok(looked) if backfilled => {
            report_update(looked);
            true
        }
        Ok(looked) => {
            finish_backfill(app, looked).await;
            true
        }
        Err(error) => {
            report_failure(&error);
            backfilled
        }
    }
}

async fn finish_backfill(app: &App, looked: usize) {
    if looked > 0 {
        optimize(app).await;
    }
    report_ready(app, looked).await;
}

fn report_update(looked: usize) {
    if looked > 0 {
        tracing::info!(blobs = looked, "content index updated");
    }
}

fn report_failure(error: &StoreError) {
    tracing::warn!(%error, "content indexing failed; retrying later");
}

async fn wait_for_work(app: &App) {
    tokio::select! {
        () = app.search_indexer.wake.notified() => {}
        () = tokio::time::sleep(TICK) => {}
    }
}

/// Merges the FTS b-trees, so queries read fewer pages.
async fn optimize(app: &App) {
    if let Err(error) = app.run_store(|store| store.search_optimize()).await {
        tracing::warn!(%error, "content index optimize failed");
    }
}

async fn report_ready(app: &App, indexed: usize) {
    match app.run_store(|store| store.search_documents()).await {
        Ok(documents) => tracing::info!(
            documents,
            indexed,
            pending = app.search_indexer.pending(),
            "content index ready"
        ),
        Err(error) => tracing::warn!(%error, "content index count failed"),
    }
}

async fn collect_garbage(app: &App) {
    match app.run_store(|store| store.search_collect_garbage()).await {
        Ok(removed) if removed > 0 => {
            tracing::info!(removed, "content index dropped unreferenced documents");
        }
        Ok(_) => {}
        Err(error) => tracing::warn!(%error, "content index garbage collection failed"),
    }
}

/// Indexes every blob waiting, a batch at a time, and reports how many it
/// looked at. Afterwards [`Indexer::pending`] is current. A blob that cannot
/// be read is left for the next pass, since its file may be mid-deletion;
/// a pass that records nothing stops rather than retrying at once.
pub async fn index_pending(app: &App) -> Result<usize, StoreError> {
    let indexer = &app.search_indexer;
    let mut pending = app.run_store(|store| store.search_pending()).await?;
    indexer.pending.store(pending, Ordering::Relaxed);
    let mut looked = 0;
    loop {
        let batch = app.run_store(|store| store.search_unindexed(BATCH)).await?;
        if batch.is_empty() {
            break;
        }
        let store = app.store.clone();
        let documents = tokio::task::spawn_blocking(move || extract_batch(&store, &batch))
            .await
            .expect("extraction does not panic");
        let recorded = documents.len();
        if recorded == 0 {
            break;
        }
        app.run_store(move |store| store.index_search_documents(&documents))
            .await?;
        looked += recorded;
        pending = pending.saturating_sub(u64::try_from(recorded).expect("count fits in u64"));
        indexer.pending.store(pending, Ordering::Relaxed);
        tokio::task::yield_now().await;
    }
    pending = app.run_store(|store| store.search_pending()).await?;
    indexer.pending.store(pending, Ordering::Relaxed);
    Ok(looked)
}

/// Reads and extracts a batch; a blob too large to parse is recorded as
/// skipped without being read.
fn extract_batch(store: &Store, batch: &[Unindexed]) -> Vec<(ContentHash, Option<Document>)> {
    batch
        .iter()
        .filter_map(|blob| {
            if blob.size.cast_unsigned() > MAX_BLOB_BYTES {
                return Some((blob.hash, None));
            }
            match std::fs::read(store.blob_path(blob.hash)) {
                Ok(bytes) => Some((blob.hash, extract::extract(&blob.path, &bytes))),
                Err(error) => {
                    tracing::warn!(%error, path = %blob.path, "content index could not read a blob");
                    None
                }
            }
        })
        .collect()
}
