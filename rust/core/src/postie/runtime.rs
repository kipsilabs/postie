//! Port of `pkg/postie/runtime.go`: the process-wide resources shared by every job — the PAR2
//! scheduler, the upload engine, the durable transfer store with its verification service, the PAR2
//! sweeper and the in-use PAR2 registry.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;
use tokio_util::sync::CancellationToken;

use super::script::{ScriptQueue, run_post_upload_script};
use crate::config::{ConfigData, PostCheck, PostUploadScriptConfig};
use crate::engine::Engine;
use crate::manifest;
use crate::par2::existing::work_dir;
use crate::par2::prefetch::Prefetcher;
use crate::par2::scheduler::Scheduler;
use crate::par2::sweep::Sweeper;
use crate::poster::{ArticleClient, PostObserver, Reposter};
use crate::transfercleaner::{ScriptRunner, TransferCleaner};
use crate::transferstore::{Store, TransferFile};
use crate::transferwriter::Recorder;
use crate::verification::{self, EarlyChecker};

pub use super::stater::{BusyFn, PoolStater, StatLimits, stat_limits};

type ReferencedFn = Arc<dyn Fn() -> Result<Vec<String>, String> + Send + Sync>;

const PAR2_SWEEP_GRACE: Duration = Duration::from_secs(24 * 3600);
const PAR2_SWEEP_INITIAL_DELAY: Duration = Duration::from_secs(5 * 60);
const PAR2_SWEEP_INTERVAL: Duration = Duration::from_secs(3600);

pub fn verification_config(pc: &PostCheck) -> verification::Config {
    let nanos = |d: &crate::config::Duration| chrono::Duration::nanoseconds(d.nanos());
    verification::Config {
        stat_batch_size: usize::try_from(pc.stat_batch_size).unwrap_or(0),
        propagation_delay: nanos(&pc.retry_delay),
        max_reposts: i64::try_from(pc.max_repost).unwrap_or(i64::MAX),
        deferred_backoff: nanos(&pc.deferred_check_delay),
        max_backoff: nanos(&pc.deferred_max_backoff),
        max_deferred_checks: pc.deferred_max_retries,
        batch_size: pc.deferred_batch_size,
        ..Default::default()
    }
}

/// Runs the post-upload script once a transfer is verified. `None` when scripts are disabled.
fn post_verify_script_runner(
    store: Store,
    cfg: PostUploadScriptConfig,
    q: Option<Arc<dyn ScriptQueue>>,
) -> Option<ScriptRunner> {
    if !cfg.enabled || cfg.command.is_empty() {
        return None;
    }
    Some(Arc::new(
        move |_transfer_id: &str, files: &[TransferFile]| {
            let (store, cfg, q) = (store.clone(), cfg.clone(), q.clone());
            let mut item_id = String::new();
            let mut source = String::new();
            for f in files {
                if !f.completed_item_id.is_empty() {
                    item_id.clone_from(&f.completed_item_id);
                }
                if source.is_empty() && f.file_role == manifest::ROLE_ORIGINAL {
                    source.clone_from(&f.source_path);
                }
            }
            if source.is_empty() {
                source = files
                    .first()
                    .map(|f| f.source_path.clone())
                    .unwrap_or_default();
            }
            Box::pin(async move {
                let nzb_path = store
                    .get_completed_item_nzb_path(&item_id)
                    .map_err(|e| e.to_string())?;
                run_post_upload_script(
                    &CancellationToken::new(),
                    &cfg,
                    q.as_deref(),
                    &nzb_path,
                    &source,
                    &item_id,
                )
                .await
            })
        },
    ))
}

/// Everything a Runtime needs from the outside world (pool clients stay behind the trait seam).
pub struct RuntimeDeps {
    /// The upload pool (`None` disables the upload engine and durable verification).
    pub upload: Option<Arc<dyn ArticleClient>>,
    /// The verification pool when verify-role servers exist, else `None` (the upload pool is used).
    pub verify: Option<Arc<dyn ArticleClient>>,
    pub store: Option<Store>,
    pub manifest_dir: PathBuf,
    pub script_queue: Option<Arc<dyn ScriptQueue>>,
}

pub struct Runtime {
    par2_scheduler: Arc<Scheduler>,
    upload_engine: Option<Arc<Engine>>,
    store: Option<Store>,
    manifest_dir: PathBuf,
    verify_service: Option<Arc<verification::Service>>,
    /// STATs articles while their upload still runs (dedicated verify pool only).
    early_checker: Option<Arc<EarlyChecker>>,
    par2_work_dir: String,
    par2_in_use: Mutex<HashMap<String, u32>>,
    /// The next queued job's PAR2 set, created while the current job uploads.
    par2_prefetch: Arc<Prefetcher>,
}

/// Point-in-time process-wide transfer resource usage (JSON names as Go's `RuntimeMetrics`).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeMetrics {
    pub upload_active_workers: i64,
    pub upload_queued_workers: i64,
    pub upload_worker_count: i64,
    pub upload_reserved_bytes: i64,
    pub upload_budget_bytes: i64,
    pub upload_bytes: i64,
    #[serde(serialize_with = "crate::gofloat::serialize")]
    pub upload_speed_bps: f64,
    #[serde(serialize_with = "crate::gofloat::serialize")]
    pub upload_avg_speed_bps: f64,
    pub par2_active_jobs: i64,
    pub par2_queued_jobs: i64,
    pub par2_capacity: usize,
}

impl Runtime {
    pub fn new(cfg: &ConfigData, deps: RuntimeDeps) -> Arc<Self> {
        let par2_work_dir = work_dir(&cfg.par2);
        let max_jobs = cfg.par2.max_concurrent_jobs.max(1);
        let capacity = deps.upload.as_ref().map_or(0, |u| u.max_connections());
        let upload_engine = (capacity > 0).then(|| {
            Arc::new(Engine::new(
                cfg.posting.article_size_in_bytes,
                cfg.posting.upload_buffer_memory_limit,
                capacity as i64,
            ))
        });
        let mut verify_service = None;
        let mut early_checker = None;
        if let (Some(store), Some(upload)) = (&deps.store, &deps.upload) {
            let shared = deps.verify.is_none();
            let verify = deps.verify.clone().unwrap_or_else(|| upload.clone());
            let reposter = Reposter::new(
                Some(upload.clone()),
                upload_engine.clone().unwrap_or_else(|| {
                    Arc::new(Engine::new(
                        cfg.posting.article_size_in_bytes,
                        cfg.posting.upload_buffer_memory_limit,
                        1,
                    ))
                }),
                cfg.posting.throttle_rate,
            );
            // Two predicates on a shared pool: STATs narrow whenever an upload is running (they
            // queue behind its article bodies), while whole cycles are skipped only once uploads
            // saturate the pool.
            let (uploading, saturated): (Option<BusyFn>, Option<BusyFn>) = match &upload_engine {
                Some(engine) if shared => {
                    let (a, q) = (engine.clone(), engine.clone());
                    (
                        Some(Arc::new(move || {
                            let m = a.metrics();
                            m.active_workers > 0 || m.queued_workers > 0
                        })),
                        Some(Arc::new(move || q.metrics().queued_workers > 0)),
                    )
                }
                _ => (None, None),
            };
            let limits = stat_limits(
                cfg.post_check.max_concurrent_checks,
                shared,
                verify.max_inflight(),
            );
            let stater: Arc<PoolStater> =
                Arc::new(PoolStater::new(verify.clone(), limits, uploading));
            let vcfg = verification_config(&cfg.post_check);
            let mut svc = verification::Service::new(
                store.clone(),
                stater.clone(),
                Arc::new(reposter),
                vcfg.clone(),
                "postie",
            );
            // Checking during upload needs its own connections, like nyuu's check connections: on a
            // shared pool those STATs would queue behind the very uploads they shadow.
            if !shared {
                let delay = vcfg
                    .with_defaults()
                    .propagation_delay
                    .to_std()
                    .unwrap_or_default();
                let ec = Arc::new(EarlyChecker::new(stater, delay));
                svc.set_early_checker(ec.clone());
                early_checker = Some(ec);
            }
            let maintain = cfg.par2.maintain_par2_files == Some(true);
            let runner = post_verify_script_runner(
                store.clone(),
                cfg.post_upload_script.clone(),
                deps.script_queue.clone(),
            );
            svc.set_cleaner(Arc::new(TransferCleaner::new(
                store.clone(),
                maintain,
                runner,
            )));
            if let Some(b) = saturated {
                svc.set_busy_check(b);
            }
            verify_service = Some(Arc::new(svc));
        }
        Arc::new_cyclic(|me: &std::sync::Weak<Self>| {
            // prefetched sets are kept from the sweeper like a running job's (weak: no cycle)
            let me = me.clone();
            let hold = Arc::new(move |paths: &[String], keep: bool| {
                if let Some(rt) = me.upgrade() {
                    if keep {
                        rt.reserve_par2(paths);
                    } else {
                        rt.release_par2(paths);
                    }
                }
            });
            Self {
                par2_scheduler: Arc::new(Scheduler::new(max_jobs)),
                upload_engine,
                store: deps.store,
                manifest_dir: deps.manifest_dir,
                verify_service,
                early_checker,
                par2_work_dir,
                par2_in_use: Mutex::new(HashMap::new()),
                par2_prefetch: Arc::new(Prefetcher::new(hold)),
            }
        })
    }

    fn in_use(&self) -> std::sync::MutexGuard<'_, HashMap<String, u32>> {
        self.par2_in_use.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Keeps PAR2 files out of the sweeper's reach until released (reference counted).
    pub fn reserve_par2(&self, paths: &[String]) {
        let mut m = self.in_use();
        for p in paths {
            *m.entry(p.clone()).or_insert(0) += 1;
        }
    }

    pub fn release_par2(&self, paths: &[String]) {
        let mut m = self.in_use();
        for p in paths {
            match m.get_mut(p) {
                Some(n) if *n > 1 => *n -= 1,
                _ => {
                    m.remove(p);
                }
            }
        }
    }

    pub fn par2_in_use(&self) -> Vec<String> {
        self.in_use().keys().cloned().collect()
    }

    /// Sweeps orphaned PAR2 files from the work dir until cancelled (no-op without a work dir).
    pub async fn run_par2_sweeper(self: Arc<Self>, cancel: CancellationToken) {
        if self.par2_work_dir.is_empty() {
            return;
        }
        let referenced: Option<ReferencedFn> = self
            .store
            .clone()
            .map(|s| Arc::new(move || s.list_source_paths().map_err(|e| e.to_string())) as _);
        let me = self.clone();
        let in_use: Arc<dyn Fn() -> Vec<String> + Send + Sync> = Arc::new(move || me.par2_in_use());
        let sweeper = Arc::new(Sweeper::new(
            &self.par2_work_dir,
            PAR2_SWEEP_GRACE,
            referenced,
            Some(in_use),
        ));
        sweeper
            .run(cancel, PAR2_SWEEP_INITIAL_DELAY, PAR2_SWEEP_INTERVAL)
            .await;
    }

    pub fn par2_scheduler(&self) -> Arc<Scheduler> {
        self.par2_scheduler.clone()
    }

    pub fn par2_prefetcher(&self) -> Arc<Prefetcher> {
        self.par2_prefetch.clone()
    }

    /// The PAR2 work directory (`""` when sets are written next to their sources).
    pub fn par2_work_dir(&self) -> &str {
        &self.par2_work_dir
    }

    pub fn upload_engine(&self) -> Option<Arc<Engine>> {
        self.upload_engine.clone()
    }

    pub fn durable_verification_enabled(&self) -> bool {
        self.verify_service.is_some()
    }

    /// Runs the durable verification loop until cancelled (no-op without a service).
    pub async fn run_verification(&self, cancel: &CancellationToken) {
        let Some(s) = &self.verify_service else {
            return;
        };
        match &self.early_checker {
            Some(ec) => {
                tokio::join!(s.run(cancel), ec.run(cancel));
            }
            None => s.run(cancel).await,
        }
    }

    /// The observer jobs hand posted articles to, when checking during upload is on.
    pub fn post_observer(&self) -> Option<Arc<dyn PostObserver>> {
        self.early_checker
            .clone()
            .map(|e| e as Arc<dyn PostObserver>)
    }

    /// Tells the verification loop a transfer was just marked uploaded.
    pub fn wake_verification(&self) {
        if let Some(s) = &self.verify_service {
            s.wake();
        }
    }

    pub fn transfer_store(&self) -> Option<&Store> {
        self.store.as_ref()
    }

    /// Drops a transfer's rows and manifests (a job that will never complete).
    pub fn discard_transfer(&self, transfer_id: &str) -> Result<(), String> {
        let Some(store) = &self.store else {
            return Ok(());
        };
        if transfer_id.is_empty() {
            return Ok(());
        }
        store
            .delete_failures_by_transfer(transfer_id)
            .map_err(|e| format!("discard transfer {transfer_id}: delete failures: {e}"))?;
        store
            .delete_files_by_transfer(transfer_id)
            .map_err(|e| format!("discard transfer {transfer_id}: delete files: {e}"))?;
        if !self.manifest_dir.as_os_str().is_empty() {
            match std::fs::remove_dir_all(self.manifest_dir.join(transfer_id)) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    return Err(format!(
                        "discard transfer {transfer_id}: remove manifests: {e}"
                    ));
                }
            }
        }
        Ok(())
    }

    /// The per-job manifest recorder; `None` without a store or transfer id (standalone mode).
    pub fn new_manifest_recorder(&self, transfer_id: &str) -> Option<Arc<Recorder>> {
        let store = self.store.clone()?;
        (!transfer_id.is_empty())
            .then(|| Arc::new(Recorder::new(transfer_id, &self.manifest_dir, store)))
    }

    /// Items awaiting verification and where each stands (empty without durable verification).
    pub fn verification_activity(&self) -> Vec<verification::Activity> {
        let Some(svc) = &self.verify_service else {
            return Vec::new();
        };
        svc.activity().unwrap_or_else(|e| {
            crate::logx::warn(
                "Failed to read verification progress",
                &[("error", e.to_string().into())],
            );
            Vec::new()
        })
    }

    pub fn metrics(&self) -> RuntimeMetrics {
        let mut m = RuntimeMetrics {
            par2_active_jobs: self.par2_scheduler.active(),
            par2_queued_jobs: self.par2_scheduler.queued(),
            par2_capacity: self.par2_scheduler.capacity(),
            ..Default::default()
        };
        if let Some(e) = &self.upload_engine {
            let em = e.metrics();
            m.upload_active_workers = em.active_workers;
            m.upload_queued_workers = em.queued_workers;
            m.upload_worker_count = em.worker_count;
            m.upload_reserved_bytes = em.reserved_bytes;
            m.upload_budget_bytes = em.budget_bytes;
            m.upload_bytes = em.upload_bytes;
            m.upload_speed_bps = em.upload_speed;
            m.upload_avg_speed_bps = em.upload_avg_speed;
        }
        m
    }
}
