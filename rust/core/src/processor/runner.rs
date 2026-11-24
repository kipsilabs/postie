//! Port of `internal/processor/processor.go`: pulls jobs off the durable queue, runs each through
//! [`Postie`] on a bounded number of slots, and settles the outcome (complete, deferred, retry or
//! error), with pause/resume, provider-availability auto-blocking and graceful shutdown.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio_util::sync::CancellationToken;

use super::availability::{ProviderHealth, evaluate_provider_availability};
use super::slots::SlotRunner;
use super::utils::{is_symlink, is_within_path};
use crate::config::{ConfigData, QueueConfig};
use crate::fileinfo::FileInfo;
use crate::par2::binary::new_executor;
use crate::par2::executor::parse_redundancy_percentage;
use crate::par2::prefetch::Discarded;
use crate::par2::scheduler::ScheduledExecutor;
use crate::pausable::PauseGate;
use crate::poster::{ArticleClient, DeferredCheckError};
use crate::postie::runtime::{Runtime, RuntimeDeps, RuntimeMetrics};
use crate::postie::script::ScriptQueue;
use crate::postie::{Postie, PostieError, par2_request};
use crate::progress::{JobProgress, ProgressState};
use crate::queue::{FileJob, PendingArticleCheck, Queue};
use crate::transferstore::Store;

/// Called with the file name and message when a job fails permanently.
pub type JobErrorFn = Arc<dyn Fn(&str, &str) + Send + Sync>;

pub const MAX_RETRIES: i64 = 3;
const PROVIDER_CHECK_INTERVAL: Duration = Duration::from_secs(30);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(30);
const FOLDER_PREFIX: &str = "FOLDER:";

/// The NNTP pool manager as the processor sees it. The real manager (built from the configured
/// servers) lives behind this seam so the processor is testable and independent of the NNTP crate.
pub trait PoolManager: Send + Sync {
    fn upload_client(&self) -> Option<Arc<dyn ArticleClient>>;
    /// The verification pool when verify-role servers exist, else `None`.
    fn verify_client(&self) -> Option<Arc<dyn ArticleClient>>;
    fn provider_health(&self) -> Result<Vec<ProviderHealth>, String>;
}

pub struct ProcessorOptions {
    pub queue: Queue,
    pub config: Arc<ConfigData>,
    pub queue_config: QueueConfig,
    pub pool: Option<Arc<dyn PoolManager>>,
    pub output_folder: String,
    pub delete_original_file: bool,
    /// Delay before deleting original files.
    pub delete_delay: Duration,
    pub maintain_original_extension: bool,
    pub watch_folder: String,
    pub follow_symlinks: bool,
    /// Consulted before each dispatch (e.g. pending config changes hold new jobs).
    pub can_process_next_item: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    pub on_job_error: Option<JobErrorFn>,
    pub on_job_complete: Option<Arc<dyn Fn() + Send + Sync>>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunningJobDetails {
    pub id: String,
    pub path: String,
    #[serde(rename = "fileName")]
    pub file_name: String,
    pub size: i64,
    pub progress: Vec<ProgressState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunningJobItem {
    pub id: String,
}

struct RunningJob {
    id: String,
    path: String,
    file_name: String,
    size: i64,
    progress: Arc<JobProgress>,
    cancel: CancellationToken,
    gate: PauseGate,
}

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("context canceled")]
    Canceled,
    #[error("{0}")]
    Msg(String),
}

impl From<PostieError> for ProcessError {
    fn from(e: PostieError) -> Self {
        match e {
            PostieError::Canceled => Self::Canceled,
            other => Self::Msg(other.to_string()),
        }
    }
}

/// A processed job: the NZB path, the job's Postie (for the script and reuse queries) and the non-fatal
/// deferred-check error when some articles still need verification.
/// Free space a PAR2 prefetch leaves untouched in the work dir, on top of room for the set itself.
const PREFETCH_DISK_RESERVE: u64 = 1 << 30;

/// Rough size of a PAR2 set for `input_bytes` at `redundancy`: the recovery data plus a margin for
/// slice rounding, packet headers and the index file.
fn prefetch_set_estimate(input_bytes: u64, redundancy: &str) -> u64 {
    let pct = parse_redundancy_percentage(redundancy).max(0.0);
    let recovery = input_bytes as f64 * pct / 100.0;
    (recovery * 1.02) as u64 + (1 << 20)
}

/// A prefetch adds a second set to the work dir next to the uploading job's, so it needs room for
/// the set twice over plus a fixed reserve; unknown free space does not block it.
fn prefetch_fits(available: Option<u64>, estimate: u64) -> bool {
    available.is_none_or(|a| {
        a >= estimate
            .saturating_mul(2)
            .saturating_add(PREFETCH_DISK_RESERVE)
    })
}

struct JobInputs {
    files: Vec<FileInfo>,
    input_folder: String,
    folder_path: Option<String>,
}

struct Processed {
    nzb_path: String,
    postie: Arc<Postie>,
    deferred: Option<DeferredCheckError>,
}

#[derive(Default)]
struct AutoPause {
    paused: bool,
    blocking_new: bool,
    reason: String,
}

pub struct Processor {
    queue: Queue,
    config: Arc<ConfigData>,
    cfg: QueueConfig,
    pool: Option<Arc<dyn PoolManager>>,
    output_folder: String,
    slots: SlotRunner,
    delete_original_file: bool,
    delete_delay: Duration,
    watch_folder: String,
    follow_symlinks: bool,
    running: Mutex<HashMap<String, Arc<RunningJob>>>,
    /// Jobs claimed from the queue but not yet running: keeps the watcher from re-adding the path.
    reserved: RwLock<HashMap<String, Instant>>,
    paused: AtomicBool,
    auto: Mutex<AutoPause>,
    provider_baseline: Mutex<HashMap<String, i64>>,
    shutting_down: AtomicBool,
    can_process_next_item: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    on_job_error: Option<JobErrorFn>,
    on_job_complete: Option<Arc<dyn Fn() + Send + Sync>>,
    runtime: Option<Arc<Runtime>>,
    monitor_cancel: CancellationToken,
    verification_started: std::sync::Once,
    jobs_done: tokio::sync::Notify,
    /// A predecessor's PAR2 prefetch, removed before this processor dispatches its first job.
    inherited_prefetch: Mutex<Option<Discarded>>,
    /// Orders a prefetch's peek-and-start against a job's receive-and-claim, so a prefetch is never
    /// started for a job that has already been taken off the queue.
    prefetch_gate: tokio::sync::Mutex<()>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

fn file_name_of(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or(path).to_string()
}

/// The job's source files plus the pre-existing PAR2 files it posted from those sources' own
/// directories (the watcher no longer queues such PAR2 files separately, issue #184, so leaving them
/// would make the next scan post them again). PAR2 reused from elsewhere is not an original.
pub fn originals_to_delete(sources: &[FileInfo], reused_par2: &[String]) -> Vec<String> {
    let clean = |p: &str| -> PathBuf { Path::new(p).components().collect() };
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut dirs = std::collections::HashSet::new();
    for f in sources {
        let key = clean(&f.path);
        dirs.insert(key.parent().map(Path::to_path_buf).unwrap_or_default());
        if seen.insert(key) {
            out.push(f.path.clone());
        }
    }
    for p in reused_par2 {
        let key = clean(p);
        if seen.contains(&key)
            || !dirs.contains(&key.parent().map(Path::to_path_buf).unwrap_or_default())
        {
            continue;
        }
        seen.insert(key);
        out.push(p.clone());
    }
    out
}

impl Processor {
    pub fn new(opts: ProcessorOptions) -> Arc<Self> {
        let monitor_cancel = CancellationToken::new();
        // The durable transfer store and manifest dir enable manifest recording and crash-safe
        // verification; manifests live next to the database file.
        let store = Store::new(opts.queue.db().clone());
        let manifest_dir = Path::new(&opts.config.database.database_path)
            .parent()
            .map(|d| d.join("transfer-manifests"))
            .unwrap_or_else(|| PathBuf::from("transfer-manifests"));
        match store.migrate_legacy_pending_checks() {
            Ok(n) if n > 0 => crate::logx::info(
                &format!(
                    "Migrated legacy pending article checks to durable verification: count={n}"
                ),
                &[("component", "processor".into())],
            ),
            Ok(_) => {}
            Err(e) => crate::logx::warn(
                &format!("Failed to migrate legacy pending article checks: {e}"),
                &[("component", "processor".into())],
            ),
        }
        let script_queue: Arc<dyn ScriptQueue> = Arc::new(opts.queue.clone());
        let runtime = Some(Runtime::new(
            &opts.config,
            RuntimeDeps {
                upload: opts.pool.as_ref().and_then(|p| p.upload_client()),
                verify: opts.pool.as_ref().and_then(|p| p.verify_client()),
                store: Some(store),
                manifest_dir,
                script_queue: Some(script_queue),
            },
        ));
        let p = Arc::new(Self {
            queue: opts.queue,
            config: opts.config,
            cfg: opts.queue_config,
            pool: opts.pool,
            output_folder: opts.output_folder,
            slots: SlotRunner::new(),
            delete_original_file: opts.delete_original_file,
            delete_delay: opts.delete_delay,
            watch_folder: opts.watch_folder,
            follow_symlinks: opts.follow_symlinks,
            running: Mutex::new(HashMap::new()),
            reserved: RwLock::new(HashMap::new()),
            paused: AtomicBool::new(false),
            auto: Mutex::new(AutoPause::default()),
            provider_baseline: Mutex::new(HashMap::new()),
            shutting_down: AtomicBool::new(false),
            can_process_next_item: opts.can_process_next_item,
            on_job_error: opts.on_job_error,
            on_job_complete: opts.on_job_complete,
            runtime,
            monitor_cancel,
            verification_started: std::sync::Once::new(),
            jobs_done: tokio::sync::Notify::new(),
            inherited_prefetch: Mutex::new(None),
            prefetch_gate: tokio::sync::Mutex::new(()),
        });
        if p.pool.is_some() {
            let me = p.clone();
            tokio::spawn(async move { me.monitor_provider_availability().await });
        }
        if let Some(rt) = &p.runtime {
            // weak: the runtime is owned by the processor, so a strong ref would be a cycle
            let me = Arc::downgrade(&p);
            rt.par2_prefetcher().set_trigger(Arc::new(move || {
                if let Some(me) = me.upgrade() {
                    drop(me.spawn_prefetch_next());
                }
            }));
        }
        p
    }

    fn durable_mode(&self) -> bool {
        self.runtime
            .as_ref()
            .is_some_and(|r| r.durable_verification_enabled())
    }

    /// Whether the durable verification path is active (the legacy post-check worker must not run).
    pub fn durable_mode_enabled(&self) -> bool {
        self.durable_mode()
    }

    pub fn transfer_runtime_metrics(&self) -> RuntimeMetrics {
        self.runtime
            .as_ref()
            .map(|r| r.metrics())
            .unwrap_or_default()
    }

    /// Runs the queue loop until `cancel` fires. Also starts, once, the durable verification service and
    /// the PAR2 sweeper (independent of pause).
    pub async fn start(self: &Arc<Self>, cancel: &CancellationToken) {
        self.verification_started.call_once(|| {
            if let Some(rt) = &self.runtime {
                let (rt1, c1) = (rt.clone(), cancel.clone());
                tokio::spawn(async move { rt1.run_verification(&c1).await });
                tokio::spawn(rt.clone().run_par2_sweeper(cancel.clone()));
            }
        });
        // a predecessor's prefetch could share paths with the jobs this processor is about to run
        let inherited = lock(&self.inherited_prefetch).take();
        if let Some(p) = inherited {
            p.discard().await;
        }
        let mut tick = tokio::time::interval(Duration::from_secs(2));
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                _ = tick.tick() => self.process_queue_items(cancel),
                () = self.slots.woken() => self.process_queue_items(cancel),
            }
        }
    }

    /// Paused, auto-blocked or held by the callback: no new job may start.
    fn dispatch_held(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
            || lock(&self.auto).blocking_new
            || self.can_process_next_item.as_ref().is_some_and(|f| !f())
    }

    /// Refills free slots unless paused, auto-blocked or held by the callback.
    pub fn process_queue_items(self: &Arc<Self>, cancel: &CancellationToken) {
        if self.dispatch_held() {
            return;
        }
        let limit = usize::try_from(self.cfg.max_concurrent_uploads).unwrap_or(0);
        let me = self.clone();
        let c = cancel.clone();
        self.slots.fill(limit, move || {
            let (me, c) = (me.clone(), c.clone());
            async move {
                if let Err(e) = me.process_next_item(&c).await
                    && !matches!(e, ProcessError::Canceled)
                {
                    crate::logx::error(
                        &format!("Error processing item: {e}"),
                        &[("component", "processor".into())],
                    );
                }
            }
        });
    }

    fn reserve_path(&self, path: &str) {
        self.reserved
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(path.to_string(), Instant::now());
    }

    fn unreserve_path(&self, path: &str) {
        self.reserved
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .remove(path);
    }

    /// Claims one job, processes it and settles the outcome.
    pub async fn process_next_item(
        self: &Arc<Self>,
        cancel: &CancellationToken,
    ) -> Result<(), ProcessError> {
        if cancel.is_cancelled() {
            return Err(ProcessError::Canceled);
        }
        if self.shutting_down.load(Ordering::SeqCst) {
            return Ok(());
        }
        if self.pool.is_none() {
            crate::logx::warn(
                "Pool manager is not available, waiting for server configuration",
                &[("component", "processor".into())],
            );
            return Ok(());
        }
        // aggregate-size threshold: hold off until the pending bytes meet the floor (0 disables)
        if self.cfg.min_size_to_start > 0 {
            match self.queue.pending_total_size() {
                Ok(pending) if pending < self.cfg.min_size_to_start => return Ok(()),
                Ok(_) => {}
                Err(e) => crate::logx::warn(
                    &format!("Failed to read pending queue size, processing anyway: {e}"),
                    &[("component", "processor".into())],
                ),
            }
        }
        let gate = self.prefetch_gate.lock().await;
        let received = self
            .queue
            .receive_file()
            .map_err(|e| ProcessError::Msg(e.to_string()))?;
        let Some(rj) = received else { return Ok(()) };
        let (msg_id, mut job) = (rj.msg_id, rj.job);
        // reserve immediately to close the gap between receive and the running-jobs entry
        self.reserve_path(&job.path);
        // released on every exit, including a panic before the running-jobs entry exists
        let _reservation = PathReservation {
            me: self,
            path: job.path.clone(),
        };
        // only a real item wakes the dispatcher; an empty receive must not, or an idle queue would spin
        let _wake = WakeOnDrop(&self.slots);
        // a prefetch for another job (queue reordered or item removed) goes before this job writes PAR2,
        // so its files cannot collide with this job's set
        if let Some(rt) = &self.runtime
            && let Some(stale) = rt.par2_prefetcher().take_unless_owner(&msg_id)
        {
            stale.discard().await;
        }
        drop(gate);
        crate::logx::info(
            &format!(
                "Processing file: msg={msg_id} path={} priority={}",
                job.path, job.priority
            ),
            &[("component", "processor".into())],
        );

        let result = self.process_file(cancel, &msg_id, &job).await;
        match result {
            Ok(p) if p.deferred.is_some() => {
                let de = p.deferred.clone().unwrap_or(DeferredCheckError {
                    failed_articles: Vec::new(),
                    total_articles: 0,
                });
                self.settle_deferred(&msg_id, &job, &p, &de)
            }
            Err(e) => {
                self.unreserve_path(&job.path);
                if matches!(e, ProcessError::Canceled) {
                    crate::logx::info(
                        &format!("Job cancelled: msg={msg_id} path={}", job.path),
                        &[("component", "processor".into())],
                    );
                    // only a user cancellation cleans up; on app shutdown keep in_progress for crash recovery.
                    // `close()` cancels the running jobs before the dispatcher's token fires, so the flag
                    // is what tells the two apart
                    if !cancel.is_cancelled() && !self.shutting_down.load(Ordering::SeqCst) {
                        if let Err(ce) = self.queue.cancel_file(&msg_id) {
                            crate::logx::warn(
                                &format!(
                                    "Failed to clean up in-progress item after cancel: {ce} id={msg_id}"
                                ),
                                &[("component", "processor".into())],
                            );
                        }
                        if let Some(rt) = &self.runtime
                            && let Err(de) = rt.discard_transfer(&job.transfer_id)
                        {
                            crate::logx::warn(
                                &format!(
                                    "Failed to discard transfer after cancel: {de} transfer={}",
                                    job.transfer_id
                                ),
                                &[("component", "processor".into())],
                            );
                        }
                    }
                    return Ok(());
                }
                self.handle_processing_error(&msg_id, &mut job, &e.to_string());
                Ok(())
            }
            Ok(p) => self.settle_success(&msg_id, &job, &p).await,
        }
    }

    /// NZB generated but some articles could not be verified immediately (legacy inline path): complete
    /// the item and store the deferred checks for the background worker.
    fn settle_deferred(
        &self,
        msg_id: &str,
        job: &FileJob,
        p: &Processed,
        de: &DeferredCheckError,
    ) -> Result<(), ProcessError> {
        crate::logx::info(
            &format!(
                "Some articles deferred for later verification: path={} deferred_articles={} total_articles={} nzb_path={}",
                job.path,
                de.failed_articles.len(),
                de.total_articles,
                p.nzb_path
            ),
            &[("component", "processor".into())],
        );
        if let Err(e) = self.queue.complete_file(msg_id, &p.nzb_path, job) {
            crate::logx::warn(
                &format!("Error marking file as completed: {e} path={}", job.path),
                &[("component", "processor".into())],
            );
            return Err(ProcessError::Msg(e.to_string()));
        }
        let delay = chrono::Duration::nanoseconds(
            self.config.post_check.deferred_check_delay.nanos().max(0),
        );
        let next = chrono::Utc::now() + delay;
        let checks: Vec<PendingArticleCheck> = de
            .failed_articles
            .iter()
            .map(|a| PendingArticleCheck {
                id: 0,
                completed_item_id: String::new(),
                message_id: a.message_id.clone(),
                groups: serde_json::to_string(&a.groups).unwrap_or_else(|_| "[]".into()),
                status: String::new(),
                retry_count: 0,
                next_retry_at: next,
                first_failure_at: next,
                last_checked_at: None,
                created_at: next,
            })
            .collect();
        match self.queue.add_pending_article_checks(msg_id, &checks) {
            Ok(()) => {
                if let Err(e) = self
                    .queue
                    .update_completed_item_verification_status(msg_id, "pending_verification")
                {
                    crate::logx::warn(
                        &format!("Failed to update verification status: {e}"),
                        &[("component", "processor".into())],
                    );
                }
            }
            Err(e) => crate::logx::warn(
                &format!("Failed to store deferred article checks: {e}"),
                &[("component", "processor".into())],
            ),
        }
        self.finish_script_and_notify(p, job, msg_id, true);
        Ok(())
    }

    fn finish_script_and_notify(
        &self,
        p: &Processed,
        job: &FileJob,
        item_id: &str,
        run_script: bool,
    ) {
        if run_script {
            // the script runs in the background: its failure is tracked in the database for retry
            let source = job
                .path
                .strip_prefix(FOLDER_PREFIX)
                .unwrap_or(&job.path)
                .to_string();
            let (postie, nzb, id) = (p.postie.clone(), p.nzb_path.clone(), item_id.to_string());
            tokio::spawn(async move {
                if let Err(e) = postie
                    .execute_post_upload_script(&CancellationToken::new(), &nzb, &source, &id)
                    .await
                {
                    crate::logx::warn(
                        &format!("Post upload script execution failed: {e} nzbPath={nzb}"),
                        &[("component", "processor".into())],
                    );
                }
            });
        }
        if let Some(f) = &self.on_job_complete {
            f();
        }
    }

    async fn settle_success(
        &self,
        msg_id: &str,
        job: &FileJob,
        p: &Processed,
    ) -> Result<(), ProcessError> {
        // queue and transfer-store writes take the global DB lock: keep them off the async workers
        crate::blocking::in_place(|| self.settle_success_blocking(msg_id, job, p))
    }

    fn settle_success_blocking(
        &self,
        msg_id: &str,
        job: &FileJob,
        p: &Processed,
    ) -> Result<(), ProcessError> {
        let reused = p.postie.reused_source(&job.path);
        let done = if reused {
            self.queue.complete_reused_file(msg_id, &p.nzb_path, job)
        } else {
            self.queue.complete_file(msg_id, &p.nzb_path, job)
        };
        if let Err(e) = done {
            crate::logx::warn(
                &format!("Error marking file as completed: {e} path={}", job.path),
                &[("component", "processor".into())],
            );
            return Err(ProcessError::Msg(e.to_string()));
        }
        let durable = self.durable_mode();
        // In durable mode the upload is complete but verification runs in the background, so the item is
        // pending verification. Link BEFORE flipping the status: a crash in between would strand an item
        // the service can never find (it discovers work through transfer_files).
        if durable
            && !reused
            && let Some(store) = self.runtime.as_ref().and_then(|r| r.transfer_store())
        {
            match store.set_completed_item_for_transfer(&job.transfer_id, msg_id) {
                Err(e) => crate::logx::warn(
                    &format!(
                        "Failed to link completed item to transfer: {e} transfer={}",
                        job.transfer_id
                    ),
                    &[("component", "processor".into())],
                ),
                Ok(()) => {
                    if let Err(e) = self
                        .queue
                        .update_completed_item_verification_status(msg_id, "pending_verification")
                    {
                        crate::logx::warn(
                            &format!(
                                "Failed to set pending_verification status: {e} path={}",
                                job.path
                            ),
                            &[("component", "processor".into())],
                        );
                    }
                }
            }
        }
        if reused
            && let Some(rt) = &self.runtime
            && let Err(e) = rt.discard_transfer(&job.transfer_id)
        {
            crate::logx::warn(
                &format!("Failed to discard superseded upload records: {e}"),
                &[("component", "processor".into())],
            );
        }
        // In durable mode the script waits for verification (run by the transfer cleaner).
        self.finish_script_and_notify(p, job, msg_id, !durable || reused);
        Ok(())
    }

    /// Retries (re-adding the job under a new id) up to [`MAX_RETRIES`], then records it as errored.
    fn handle_processing_error(&self, msg_id: &str, job: &mut FileJob, err: &str) {
        crate::logx::error(
            &format!(
                "Error processing file: {err} path={} retryCount={} maxRetries={MAX_RETRIES} jobID={msg_id}",
                job.path, job.retry_count
            ),
            &[("component", "processor".into())],
        );
        job.retry_count += 1;
        if job.retry_count >= MAX_RETRIES {
            let name = file_name_of(&job.path);
            crate::logx::error(
                &format!(
                    "Job failed permanently after reaching max retries: path={} retryCount={}",
                    job.path, job.retry_count
                ),
                &[("component", "processor".into())],
            );
            if let Some(f) = &self.on_job_error {
                f(&name, err);
            }
            if let Err(e) = self.queue.mark_as_error(msg_id, job, err) {
                crate::logx::warn(
                    &format!("Failed to mark job as error: {e} path={}", job.path),
                    &[("component", "processor".into())],
                );
                // fallback: back into the queue, after clearing the stale in-progress row
                if let Err(ce) = self.queue.clear_in_progress(msg_id) {
                    crate::logx::warn(
                        &format!("Failed to clear in-progress row before re-add: {ce}"),
                        &[("component", "processor".into())],
                    );
                }
                if let Err(re) = self.queue.readd_job(job) {
                    crate::logx::warn(
                        &format!("Failed to re-add job to queue: {re} path={}", job.path),
                        &[("component", "processor".into())],
                    );
                }
            }
            return;
        }
        // clear the old id's in-progress row first, or the path shows up twice (pending + stale running)
        if let Err(e) = self.queue.clear_in_progress(msg_id) {
            crate::logx::warn(
                &format!(
                    "Failed to clear in-progress row before retry: {e} path={}",
                    job.path
                ),
                &[("component", "processor".into())],
            );
        }
        if let Err(e) = self.queue.readd_job(job) {
            crate::logx::warn(
                &format!(
                    "Failed to re-add job to queue for retry: {e} path={}",
                    job.path
                ),
                &[("component", "processor".into())],
            );
        }
    }

    async fn process_file(
        self: &Arc<Self>,
        cancel: &CancellationToken,
        msg_id: &str,
        job: &FileJob,
    ) -> Result<Processed, ProcessError> {
        let JobInputs {
            files,
            input_folder,
            folder_path,
        } = self.job_inputs(job)?;
        let is_folder = folder_path.is_some();
        let file_name = match &folder_path {
            Some(fp) => {
                crate::logx::info(
                    &format!(
                        "Processing folder as single NZB: folder={} files={}",
                        file_name_of(fp),
                        files.len()
                    ),
                    &[("component", "processor".into())],
                );
                file_name_of(fp)
            }
            None => file_name_of(&job.path),
        };

        let job_cancel = cancel.child_token();
        let gate = PauseGate::new(job_cancel.clone());
        let progress = Arc::new(JobProgress::new(msg_id));
        let running = Arc::new(RunningJob {
            id: msg_id.to_string(),
            path: job.path.clone(),
            file_name,
            size: job.size,
            progress: progress.clone(),
            cancel: job_cancel.clone(),
            gate: gate.clone(),
        });
        lock(&self.running).insert(msg_id.to_string(), running);
        // the path is now tracked in running jobs
        self.unreserve_path(&job.path);
        if self.paused.load(Ordering::SeqCst) {
            gate.pause();
            progress.set_all_paused(true);
        }
        let _cleanup = RunningGuard {
            me: self,
            id: msg_id.to_string(),
            cancel: job_cancel.clone(),
        };

        let Some(pool) = &self.pool else {
            return Err(ProcessError::Msg(format!(
                "pool manager is not available for job {msg_id}"
            )));
        };
        let Some(upload) = pool.upload_client() else {
            return Err(ProcessError::Msg(format!(
                "failed to create postie instance for job {msg_id}: no upload pool"
            )));
        };
        let verify = pool.verify_client().unwrap_or_else(|| upload.clone());
        let queue_script: Arc<dyn ScriptQueue> = Arc::new(self.queue.clone());
        let postie = Arc::new(Postie::new(
            &self.config,
            upload,
            verify,
            progress,
            Some(queue_script),
            self.runtime.clone(),
            &job.transfer_id,
            Some(Arc::new(gate)),
        ));
        let _close = CloseOnDrop(postie.clone());

        // the job override beats the global delete-original setting
        let mut delete_original = job.delete_original.unwrap_or(self.delete_original_file);
        postie.set_delete_original(delete_original);

        // a job that creates no PAR2 of its own never reaches the prefetch trigger, so it starts the
        // next job's prefetch itself
        if par2_request(
            &self.config,
            &files,
            &input_folder,
            &self.output_folder,
            is_folder,
        )
        .is_none()
        {
            drop(self.spawn_prefetch_next());
        }
        let outcome = postie
            .post(
                &job_cancel,
                &files,
                &input_folder,
                &self.output_folder,
                is_folder,
            )
            .await?;
        if let Some(de) = outcome.deferred {
            // non-fatal: the NZB exists and the postie is valid
            return Ok(Processed {
                nzb_path: outcome.nzb_path,
                postie: postie.clone(),
                deferred: Some(de),
            });
        }
        if postie.reused_source(&job.path) {
            delete_original = false;
        }
        // In durable mode the upload is not yet verified: deleting the original would destroy the only
        // recovery source, so it is retained and deleted after verification by the transfer lifecycle.
        if delete_original && self.durable_mode() {
            crate::logx::info(
                &format!(
                    "Durable mode: retaining original files until verification succeeds: path={}",
                    job.path
                ),
                &[("component", "processor".into())],
            );
            delete_original = false;
        }
        if delete_original {
            if !self.delete_delay.is_zero() {
                crate::logx::info(
                    &format!(
                        "Waiting before deleting original files: delay={:?}",
                        self.delete_delay
                    ),
                    &[("component", "processor".into())],
                );
                tokio::select! {
                    () = tokio::time::sleep(self.delete_delay) => {}
                    // job_cancel is a child of the dispatcher token: it also fires on a user cancel
                    () = job_cancel.cancelled() => {
                        crate::logx::info("Cancelled while waiting to delete files; originals kept", &[("component", "processor".into())]);
                        return Ok(Processed { nzb_path: outcome.nzb_path, postie: postie.clone(), deferred: None });
                    }
                }
            }
            for path in originals_to_delete(&files, &postie.reused_par2_files()) {
                if let Err(e) = std::fs::remove_file(&path)
                    && e.kind() != std::io::ErrorKind::NotFound
                {
                    crate::logx::warn(
                        &format!("Could not delete original file: path={path} error={e}"),
                        &[("component", "processor".into())],
                    );
                    continue;
                }
                crate::uploadtemp::prune_after_delete(Path::new(&path));
            }
        }
        Ok(Processed {
            nzb_path: outcome.nzb_path,
            postie: postie.clone(),
            deferred: None,
        })
    }

    /// What a job posts: its files, the folder their paths are relative to, and (folder jobs) the folder.
    fn job_inputs(&self, job: &FileJob) -> Result<JobInputs, ProcessError> {
        let folder_path = job.path.strip_prefix(FOLDER_PREFIX).map(str::to_string);
        let files = if let Some(fp) = &folder_path {
            let files = self.collect_files_in_folder(fp).map_err(|e| {
                ProcessError::Msg(format!("failed to collect files in folder {fp}: {e}"))
            })?;
            if files.is_empty() {
                return Err(ProcessError::Msg(format!("no files found in folder {fp}")));
            }
            files
        } else {
            vec![FileInfo {
                path: job.path.clone(),
                size: u64::try_from(job.size).unwrap_or(0),
                relative_path: String::new(),
            }]
        };
        let input_folder = if !job.input_folder.is_empty() {
            job.input_folder.clone()
        } else if let Some(fp) = &folder_path {
            Path::new(fp)
                .parent()
                .map(|d| d.to_string_lossy().into_owned())
                .unwrap_or_default()
        } else if !self.watch_folder.is_empty() && is_within_path(&job.path, &self.watch_folder) {
            self.watch_folder.clone()
        } else {
            Path::new(&job.path)
                .parent()
                .map(|d| d.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        Ok(JobInputs {
            files,
            input_folder,
            folder_path,
        })
    }

    /// Whether a prefetch could apply at all: PAR2 on, one upload slot (more slots already overlap
    /// jobs), a PAR2 work dir, and a processor that is not shutting down.
    fn prefetch_possible(&self) -> bool {
        self.runtime
            .as_ref()
            .is_some_and(|rt| !rt.par2_work_dir().is_empty())
            && self.config.par2.enabled != Some(false)
            && self.cfg.max_concurrent_uploads == 1
            && !self.shutting_down.load(Ordering::SeqCst)
    }

    /// Plans the next job's PAR2 prefetch on a blocking thread: it reads the queue, walks a folder job's
    /// tree and stats every input, none of which may hold up the calling job's upload.
    fn spawn_prefetch_next(self: &Arc<Self>) -> Option<tokio::task::JoinHandle<()>> {
        if !self.prefetch_possible() {
            return None;
        }
        let me = self.clone();
        Some(tokio::task::spawn_blocking(move || me.prefetch_next_par2()))
    }

    /// Starts creating the next queued job's PAR2 set while the current job uploads, so one upload
    /// slot does not leave the uplink idle during parity creation. Runs after a job's own PAR2 call
    /// (or at the start of a job that makes none), on a blocking thread. Only when a new job could
    /// start, only into Postie's own work dir, never over a set of the same name already there, and
    /// only when the disk has room for the extra set.
    fn prefetch_next_par2(&self) {
        let Some(rt) = &self.runtime else { return };
        if !self.prefetch_possible() || self.dispatch_held() {
            return;
        }
        let _gate = self.prefetch_gate.blocking_lock();
        if self.cfg.min_size_to_start > 0
            && self
                .queue
                .pending_total_size()
                .is_ok_and(|n| n < self.cfg.min_size_to_start)
        {
            return;
        }
        let next = match self.queue.peek_file() {
            Ok(Some(next)) => next,
            Ok(None) => return,
            Err(e) => {
                crate::logx::warn(
                    &format!("Could not look at the next queued job for PAR2 prefetch: {e}"),
                    &[("component", "processor".into())],
                );
                return;
            }
        };
        let Ok(inputs) = self.job_inputs(&next.job) else {
            return;
        };
        let Some(request) = par2_request(
            &self.config,
            &inputs.files,
            &inputs.input_folder,
            &self.output_folder,
            inputs.folder_path.is_some(),
        ) else {
            return;
        };
        let store = rt.par2_prefetcher();
        if store.owner().as_deref() == Some(next.msg_id.as_str()) {
            return;
        }
        let main = Path::new(rt.par2_work_dir()).join(format!("{}.par2", request.set_base()));
        if !request.output_dir().is_empty() || main.exists() {
            return;
        }
        let input_bytes = request.files().iter().map(|f| f.size).sum();
        let estimate = prefetch_set_estimate(input_bytes, &self.config.par2.redundancy);
        let available = crate::diskspace::available_bytes(Path::new(rt.par2_work_dir()));
        if !prefetch_fits(available, estimate) {
            crate::logx::info(
                &format!(
                    "Not creating the next job's PAR2 early: too little free space in the PAR2 work dir: needed={} available={available:?}",
                    estimate
                        .saturating_mul(2)
                        .saturating_add(PREFETCH_DISK_RESERVE)
                ),
                &[("component", "processor".into())],
            );
            return;
        }
        crate::logx::info(
            &format!(
                "Creating PAR2 for the next queued job during this upload: path={}",
                next.job.path
            ),
            &[("component", "processor".into())],
        );
        let exec = ScheduledExecutor::wrap(
            new_executor(
                self.config.posting.article_size_in_bytes,
                &self.config.par2,
                None,
            ),
            Some(rt.par2_scheduler()),
        );
        if let Some(old) = store.start(&next.msg_id, request, exec) {
            tokio::spawn(old.discard());
        }
    }

    /// Recursively lists a folder's files (lexical order, like Go's `filepath.Walk`) with paths relative
    /// to the folder's parent so subjects include the folder name, `/`-separated.
    pub fn collect_files_in_folder(&self, folder: &str) -> std::io::Result<Vec<FileInfo>> {
        let parent = Path::new(folder)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default();
        let mut out = Vec::new();
        self.walk(Path::new(folder), &parent, &mut out)?;
        Ok(out)
    }

    fn walk(&self, dir: &Path, parent: &Path, out: &mut Vec<FileInfo>) -> std::io::Result<()> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for e in entries {
            let path = e.path();
            let meta = std::fs::symlink_metadata(&path)?;
            if meta.is_dir() {
                self.walk(&path, parent, out)?;
                continue;
            }
            if !self.follow_symlinks {
                match is_symlink(&path) {
                    Ok(true) | Err(_) => continue,
                    Ok(false) => {}
                }
            }
            let rel = path
                .strip_prefix(parent)
                .map(|r| {
                    r.components()
                        .map(|c| c.as_os_str().to_string_lossy().into_owned())
                        .collect::<Vec<_>>()
                        .join("/")
                })
                .unwrap_or_else(|_| file_name_of(&path.to_string_lossy()));
            out.push(FileInfo {
                path: path.to_string_lossy().into_owned(),
                size: meta.len(),
                relative_path: rel,
            });
        }
        Ok(())
    }

    /// Cancels a running job by id.
    pub fn cancel_job(&self, job_id: &str) -> Result<(), String> {
        // removed from tracking first to prevent duplicate events
        let Some(job) = lock(&self.running).remove(job_id) else {
            return Err(format!("job {job_id} is not currently running"));
        };
        job.cancel.cancel();
        crate::logx::info(
            &format!("Job cancelled: jobID={job_id}"),
            &[("component", "processor".into())],
        );
        Ok(())
    }

    pub fn get_running_jobs(&self) -> HashMap<String, bool> {
        lock(&self.running)
            .keys()
            .map(|k| (k.clone(), true))
            .collect()
    }

    pub fn get_running_job_items(&self) -> Vec<RunningJobItem> {
        lock(&self.running)
            .keys()
            .map(|k| RunningJobItem { id: k.clone() })
            .collect()
    }

    pub fn get_running_job_details(&self) -> HashMap<String, RunningJobDetails> {
        lock(&self.running)
            .values()
            .map(|j| {
                (
                    j.id.clone(),
                    RunningJobDetails {
                        id: j.id.clone(),
                        path: j.path.clone(),
                        file_name: j.file_name.clone(),
                        size: j.size,
                        progress: j.progress.get_all_progress_state(),
                    },
                )
            })
            .collect()
    }

    /// Whether `path` is being processed: reserved or running, directly or inside a `FOLDER:` job.
    pub fn is_path_being_processed(&self, path: &str) -> bool {
        let hit = |job_path: &str| {
            job_path == path
                || job_path
                    .strip_prefix(FOLDER_PREFIX)
                    .is_some_and(|f| is_within_path(path, f))
        };
        if self
            .reserved
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .any(|p| hit(p))
        {
            return true;
        }
        lock(&self.running).values().any(|j| hit(&j.path))
    }

    /// Blocks new jobs and suspends the running ones.
    pub fn pause_processing(&self) {
        if self.paused.swap(true, Ordering::SeqCst) {
            return;
        }
        for (id, j) in lock(&self.running).iter() {
            j.gate.pause();
            j.progress.set_all_paused(true);
            crate::logx::info(
                &format!("Paused running job: jobID={id}"),
                &[("component", "processor".into())],
            );
        }
        crate::logx::info(
            "Paused - new jobs blocked, active jobs suspended",
            &[("component", "processor".into())],
        );
    }

    pub fn resume_processing(&self) {
        if !self.paused.swap(false, Ordering::SeqCst) {
            return;
        }
        for (id, j) in lock(&self.running).iter() {
            j.gate.resume();
            j.progress.set_all_paused(false);
            crate::logx::info(
                &format!("Resumed running job: jobID={id}"),
                &[("component", "processor".into())],
            );
        }
        crate::logx::info(
            "Resumed - new jobs allowed, active jobs resumed",
            &[("component", "processor".into())],
        );
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    pub fn is_auto_paused(&self) -> bool {
        lock(&self.auto).paused
    }

    pub fn auto_pause_reason(&self) -> String {
        lock(&self.auto).reason.clone()
    }

    async fn monitor_provider_availability(self: Arc<Self>) {
        let mut tick = tokio::time::interval(PROVIDER_CHECK_INTERVAL);
        tick.tick().await;
        loop {
            tokio::select! {
                () = self.monitor_cancel.cancelled() => return,
                _ = tick.tick() => self.check_and_handle_provider_availability(),
            }
        }
    }

    /// Blocks new jobs while no provider is reachable (running jobs continue so connections can recover,
    /// otherwise paused jobs would make no connections and never auto-resume) and unblocks them once one
    /// looks reachable again. A manual pause is never touched.
    pub fn check_and_handle_provider_availability(&self) {
        let Some(pool) = &self.pool else { return };
        let stats = match pool.provider_health() {
            Ok(s) => s,
            Err(e) => {
                return crate::logx::warn(
                    &format!("Failed to get pool metrics for provider check: {e}"),
                    &[("component", "processor".into())],
                );
            }
        };
        let total = stats.len();
        let (active, next) = evaluate_provider_availability(&stats, &lock(&self.provider_baseline));
        *lock(&self.provider_baseline) = next;
        let mut a = lock(&self.auto);
        if active == 0 && total > 0 && !a.paused {
            crate::logx::warn(
                "No providers available - blocking new jobs (running jobs continue unaffected)",
                &[("component", "processor".into())],
            );
            a.blocking_new = true;
            a.paused = true;
            a.reason = "All NNTP providers are unavailable".into();
        } else if active > 0 && a.paused {
            // also fires as a probe: with no jobs running no new errors accrue, so a dead provider looks
            // reachable until the next job tries it; never unblocking is the failure mode this replaced
            crate::logx::info(
                &format!(
                    "No new provider errors since last check - allowing new jobs: activeProviders={active}"
                ),
                &[("component", "processor".into())],
            );
            a.blocking_new = false;
            a.paused = false;
            a.reason.clear();
        }
    }

    /// Cancels every running job and waits up to 30 s for them to finish.
    /// Stops prefetching (this processor is being replaced) and returns its held PAR2 prefetch, which
    /// the replacement removes before it starts: `close` runs concurrently with the new processor and
    /// would otherwise remove it while the new one may already be writing the same set.
    pub fn stop_par2_prefetch(&self) -> Option<Discarded> {
        self.shutting_down.store(true, Ordering::SeqCst);
        self.runtime
            .as_ref()
            .and_then(|rt| rt.par2_prefetcher().close())
    }

    /// Takes over a replaced processor's prefetch; see [`Processor::stop_par2_prefetch`].
    pub fn inherit_par2_discard(&self, prefetch: Discarded) {
        *lock(&self.inherited_prefetch) = Some(prefetch);
    }

    pub async fn close(&self) {
        crate::logx::info("Shutdown initiated", &[("component", "processor".into())]);
        self.shutting_down.store(true, Ordering::SeqCst);
        self.monitor_cancel.cancel();
        // closed first, so a prefetch being planned right now cannot land after this point
        if let Some(rt) = &self.runtime
            && let Some(p) = rt.par2_prefetcher().close()
        {
            p.discard().await;
        }
        let count = {
            let running = lock(&self.running);
            running.values().for_each(|j| j.cancel.cancel());
            running.len()
        };
        if count > 0 {
            crate::logx::info(
                &format!("Waiting for running jobs to be cancelled: count={count}"),
                &[("component", "processor".into())],
            );
            let deadline = Instant::now() + CLOSE_TIMEOUT;
            while !lock(&self.running).is_empty() && Instant::now() < deadline {
                let _ = tokio::time::timeout(Duration::from_millis(100), self.jobs_done.notified())
                    .await;
            }
            if !lock(&self.running).is_empty() {
                crate::logx::warn(
                    &format!(
                        "Timeout waiting for jobs to complete: remainingJobs={}",
                        lock(&self.running).len()
                    ),
                    &[("component", "processor".into())],
                );
            }
        }
        lock(&self.running).clear();
        self.reserved
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        crate::logx::info("Shutdown completed", &[("component", "processor".into())]);
    }
}

/// Signals the dispatcher when a job slot finishes (even on early return).
struct WakeOnDrop<'a>(&'a SlotRunner);

impl Drop for WakeOnDrop<'_> {
    fn drop(&mut self) {
        self.0.signal();
    }
}

struct PathReservation<'a> {
    me: &'a Processor,
    path: String,
}

impl Drop for PathReservation<'_> {
    fn drop(&mut self) {
        self.me.unreserve_path(&self.path);
    }
}

struct RunningGuard<'a> {
    me: &'a Processor,
    id: String,
    cancel: CancellationToken,
}

impl Drop for RunningGuard<'_> {
    fn drop(&mut self) {
        lock(&self.me.running).remove(&self.id);
        self.cancel.cancel();
        self.me.jobs_done.notify_waiters();
    }
}

struct CloseOnDrop(Arc<Postie>);

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        self.0.close();
    }
}

impl Processor {
    /// Process-wide transfer runtime (shared with the backend for metrics and discard).
    pub fn runtime(&self) -> Option<&Arc<Runtime>> {
        self.runtime.as_ref()
    }

    /// The durable queue this processor drains.
    pub fn queue(&self) -> &Queue {
        &self.queue
    }
}

#[cfg(test)]
mod tests;
