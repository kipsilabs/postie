//! Port of `pkg/postie/runtime.go`: the process-wide resources shared by every job — the PAR2
//! scheduler, the upload engine, the durable transfer store with its verification service, the PAR2
//! sweeper and the in-use PAR2 registry.

use std::collections::{HashMap, HashSet};
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
use crate::par2::scheduler::Scheduler;
use crate::par2::sweep::Sweeper;
use crate::poster::{ArticleClient, BoxFut, Reposter};
use crate::transfercleaner::{ScriptRunner, TransferCleaner};
use crate::transferstore::{Store, TransferFile};
use crate::transferwriter::Recorder;
use crate::verification::{self, Stater};

type ReferencedFn = Arc<dyn Fn() -> Result<Vec<String>, String> + Send + Sync>;

const PAR2_SWEEP_GRACE: Duration = Duration::from_secs(24 * 3600);
const PAR2_SWEEP_INITIAL_DELAY: Duration = Duration::from_secs(5 * 60);
const PAR2_SWEEP_INTERVAL: Duration = Duration::from_secs(3600);

/// Adapts a pool client to the verification service's `Stater`: a present article is `Ok(false)`
/// (not missing), a confirmed absence `Ok(true)`, and an error leaves presence unknown.
pub struct PoolStater {
    client: Arc<dyn ArticleClient>,
    concurrency: usize,
}

impl PoolStater {
    pub fn new(client: Arc<dyn ArticleClient>, concurrency: usize) -> Self {
        Self {
            client,
            concurrency: concurrency.max(1),
        }
    }
}

impl Stater for PoolStater {
    fn stat<'a>(&'a self, id: &'a str) -> BoxFut<'a, Result<bool, String>> {
        Box::pin(async move {
            self.client
                .stat(id)
                .await
                .map(|present| !present)
                .map_err(|e| e.to_string())
        })
    }

    /// Ids not confirmed present; a per-id error counts as missing (the per-failure `stat` re-validates
    /// before any repost budget is spent).
    fn stat_batch<'a>(&'a self, ids: &'a [String]) -> BoxFut<'a, Result<HashSet<String>, String>> {
        Box::pin(async move {
            let sem = Arc::new(tokio::sync::Semaphore::new(self.concurrency));
            let mut set = tokio::task::JoinSet::new();
            for id in ids {
                let (client, sem, id) = (self.client.clone(), sem.clone(), id.clone());
                set.spawn(async move {
                    let _p = sem.acquire().await;
                    let present = matches!(client.stat(&id).await, Ok(true));
                    (id, present)
                });
            }
            let mut missing = HashSet::new();
            while let Some(r) = set.join_next().await {
                if let Ok((id, present)) = r
                    && !present
                {
                    missing.insert(id);
                }
            }
            Ok(missing)
        })
    }
}

/// STAT concurrency: the configured value, else 2 when verification shares the upload pool (so it does
/// not steal its connections) or 16 with a dedicated pool; capped at the verify pool's capacity.
pub fn stat_concurrency(configured: i64, shared_pool: bool, verify_capacity: usize) -> usize {
    let c = usize::try_from(configured)
        .ok()
        .filter(|c| *c > 0)
        .unwrap_or(if shared_pool { 2 } else { 16 });
    if verify_capacity > 0 {
        c.min(verify_capacity)
    } else {
        c
    }
}

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
    par2_work_dir: String,
    par2_in_use: Mutex<HashMap<String, u32>>,
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
            let stater = PoolStater::new(
                verify.clone(),
                stat_concurrency(
                    cfg.post_check.max_concurrent_checks,
                    shared,
                    verify.max_connections(),
                ),
            );
            let mut svc = verification::Service::new(
                store.clone(),
                Arc::new(stater),
                Arc::new(reposter),
                verification_config(&cfg.post_check),
                "postie",
            );
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
            if shared && let Some(engine) = upload_engine.clone() {
                svc.set_busy_check(Arc::new(move || engine.metrics().queued_workers > 0));
            }
            verify_service = Some(Arc::new(svc));
        }
        Arc::new(Self {
            par2_scheduler: Arc::new(Scheduler::new(max_jobs)),
            upload_engine,
            store: deps.store,
            manifest_dir: deps.manifest_dir,
            verify_service,
            par2_work_dir,
            par2_in_use: Mutex::new(HashMap::new()),
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

    pub fn upload_engine(&self) -> Option<Arc<Engine>> {
        self.upload_engine.clone()
    }

    pub fn durable_verification_enabled(&self) -> bool {
        self.verify_service.is_some()
    }

    /// Runs the durable verification loop until cancelled (no-op without a service).
    pub async fn run_verification(&self, cancel: &CancellationToken) {
        if let Some(s) = &self.verify_service {
            s.run(cancel).await;
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
