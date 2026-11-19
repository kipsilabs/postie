//! Port of `internal/backend/app.go`, `config.go`, `queue.go`, `processor.go` and `watcher.go`: the
//! application service. It owns the config (applied and pending), the database, queue, pool manager,
//! processor, post-check worker and watchers, and exposes the operations both the Tauri commands and the
//! web API call. Components are (re)started only when the inputs they captured changed.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde::Serialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::config_change::{pool_config_changed, processor_config_changed, watcher_config_changed};
use super::config_location::resolve_config_path;
use super::paths::AppPaths;
use crate::apikey::{self, SqlStore};
use crate::config::{self, ConfigData, ServerConfig, WatcherConfig};
use crate::database::{self, MigrationRunner};
use crate::db::Db;
use crate::pool::Manager;
use crate::postie::runtime::{PoolStater, RuntimeMetrics};
use crate::processor::postcheck_retry::PostCheckRetryWorker;
use crate::processor::runner::{
    PoolManager, Processor, ProcessorOptions, RunningJobDetails, RunningJobItem,
};
use crate::queue::Queue;
use crate::watcher::{Watcher, WatcherScheduleInfo, WatcherStatusInfo};

pub mod api;
mod arr;

type Emitter = Arc<dyn Fn(&str, Option<Value>) + Send + Sync>;

/// The frontend's server form data.
#[derive(Debug, Clone, Default, serde::Deserialize, Serialize)]
#[serde(default)]
pub struct ServerData {
    pub name: String,
    pub host: String,
    pub port: i64,
    pub username: String,
    pub password: String,
    pub ssl: bool,
    #[serde(rename = "maxConnections")]
    pub max_connections: i64,
    pub role: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidationResult {
    pub valid: bool,
    pub error: String,
}

/// Tests a server's connectivity (the NNTP implementation connects and authenticates once).
pub trait ServerTester: Send + Sync {
    fn test<'a>(&'a self, server: &'a ServerData) -> crate::poster::BoxFut<'a, ValidationResult>;
}

pub struct NntpTester;

impl ServerTester for NntpTester {
    fn test<'a>(&'a self, s: &'a ServerData) -> crate::poster::BoxFut<'a, ValidationResult> {
        Box::pin(async move {
            if s.host.is_empty() {
                return ValidationResult {
                    valid: false,
                    error: "Host is required".into(),
                };
            }
            if s.port <= 0 || s.port > 65535 {
                return ValidationResult {
                    valid: false,
                    error: "Port must be between 1 and 65535".into(),
                };
            }
            let mut ep = postie_nntp::Endpoint::new(&s.host, u16::try_from(s.port).unwrap_or(0));
            ep.tls = s.ssl;
            ep.username.clone_from(&s.username);
            ep.password.clone_from(&s.password);
            match postie_nntp::test_provider(&ep).await {
                Ok(rtt) => {
                    eprintln!(
                        "backend: provider connectivity test successful: host={} port={} rtt={rtt:?}",
                        s.host, s.port
                    );
                    ValidationResult {
                        valid: true,
                        error: String::new(),
                    }
                }
                Err(e) => {
                    eprintln!(
                        "backend: provider connectivity test failed: host={} port={} error={e}",
                        s.host, s.port
                    );
                    ValidationResult {
                        valid: false,
                        error: format!("Connection test failed: {e}"),
                    }
                }
            }
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppStatus {
    pub has_config: bool,
    pub config_path: String,
    pub uploading: bool,
    pub critical_config_error: bool,
    pub error: String,
    pub is_first_start: bool,
    pub has_servers: bool,
    pub server_count: usize,
    pub valid_server_count: usize,
    pub config_valid: bool,
    pub needs_configuration: bool,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessorStatus {
    pub has_processor: bool,
    pub running_jobs: usize,
    #[serde(rename = "runningJobIDs")]
    pub running_job_ids: Vec<String>,
}

#[derive(Default)]
struct State {
    config: Option<Arc<ConfigData>>,
    config_path: PathBuf,
    config_location_err: Option<String>,
    config_load_error: String,
    first_start: bool,
    pending: Option<ConfigData>,
    db: Option<Db>,
    pool: Option<Arc<Manager>>,
    queue: Option<Queue>,
    processor: Option<Arc<Processor>>,
    post_check: Option<PostCheckRetryWorker>,
    watchers: Vec<Arc<Watcher>>,
    watch_cancel: Option<CancellationToken>,
    proc_cancel: Option<CancellationToken>,
    critical_error: String,
}

pub struct App {
    paths: AppPaths,
    tester: Arc<dyn ServerTester>,
    emitter: Mutex<Option<Emitter>>,
    state: Mutex<State>,
    root: CancellationToken,
    applying: std::sync::atomic::AtomicBool,
    me: Mutex<Weak<App>>,
    web_mode: std::sync::atomic::AtomicBool,
    stats_cache: Mutex<api::StatsCache>,
}

fn valid_server_count(c: &ConfigData) -> usize {
    c.servers
        .iter()
        .flatten()
        .filter(|s| !s.host.is_empty())
        .count()
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl App {
    pub fn new(paths: AppPaths, tester: Arc<dyn ServerTester>) -> Arc<Self> {
        let (config_path, loc_err) = match resolve_config_path(&paths.config) {
            Ok(p) => (p, None),
            Err(e) => {
                eprintln!("backend: could not resolve saved configuration location: {e}");
                (paths.config.clone(), Some(e))
            }
        };
        let app = Arc::new(Self {
            paths,
            tester,
            emitter: Mutex::new(None),
            state: Mutex::new(State {
                config_path,
                config_location_err: loc_err,
                ..Default::default()
            }),
            root: CancellationToken::new(),
            applying: std::sync::atomic::AtomicBool::new(false),
            me: Mutex::new(Weak::new()),
            web_mode: std::sync::atomic::AtomicBool::new(false),
            stats_cache: Mutex::new(api::StatsCache::default()),
        });
        *lock(&app.me) = Arc::downgrade(&app);
        crate::logx::info(
            "Using OS-specific paths",
            &[
                ("config", app.paths.config.to_string_lossy().into()),
                ("database", app.paths.database.to_string_lossy().into()),
                ("data", app.paths.data.to_string_lossy().into()),
                ("log", app.paths.log.to_string_lossy().into()),
                (
                    "actualLogPath",
                    crate::logx::current_path().to_string_lossy().into(),
                ),
            ],
        );
        app
    }

    /// The web server keeps using the default location (relocation is desktop-only).
    pub fn set_web_mode(&self, web: bool) {
        crate::logx::info("Setting web mode", &[("isWeb", web.into())]);
        self.web_mode
            .store(web, std::sync::atomic::Ordering::SeqCst);
        if web {
            let mut s = lock(&self.state);
            s.config_path.clone_from(&self.paths.config);
            s.config_location_err = None;
        }
    }

    /// Desktop only: copies the saved configuration into `directory/config.yaml` and makes it the
    /// active location (Go's `relocateConfig`). Returns the active path.
    pub fn relocate_config(&self, directory: &str) -> Result<String, String> {
        if self.web_mode.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("configuration relocation is only available in the desktop app".into());
        }
        let current = self.config_path();
        let dest = super::config_location::relocate_config(
            &self.paths.config,
            &current,
            Path::new(directory),
        )?;
        lock(&self.state).config_path.clone_from(&dest);
        Ok(dest.to_string_lossy().into_owned())
    }

    pub fn set_event_emitter(&self, f: Emitter) {
        *lock(&self.emitter) = Some(f);
    }

    fn emit(&self, kind: &str, payload: Option<Value>) {
        if let Some(f) = lock(&self.emitter).clone() {
            f(kind, payload);
        }
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    pub fn config_path(&self) -> PathBuf {
        lock(&self.state).config_path.clone()
    }

    pub fn queue(&self) -> Option<Queue> {
        lock(&self.state).queue.clone()
    }

    pub fn processor(&self) -> Option<Arc<Processor>> {
        lock(&self.state).processor.clone()
    }

    pub fn pool_manager(&self) -> Option<Arc<Manager>> {
        lock(&self.state).pool.clone()
    }

    pub fn db(&self) -> Option<Db> {
        lock(&self.state).db.clone()
    }

    pub fn applied_config(&self) -> Option<Arc<ConfigData>> {
        lock(&self.state).config.clone()
    }

    fn save_config_file(&self, cfg: &ConfigData) -> Result<(), String> {
        let (path, err) = {
            let s = lock(&self.state);
            (s.config_path.clone(), s.config_location_err.clone())
        };
        if let Some(e) = err {
            return Err(e);
        }
        config::save_config(cfg, &path).map_err(|e| e.to_string())?;
        lock(&self.state).config_load_error.clear();
        Ok(())
    }

    /// A saved custom location must not send the user through first-run setup when unavailable.
    fn determine_first_start(&self) -> bool {
        let (path, err) = {
            let s = lock(&self.state);
            (s.config_path.clone(), s.config_location_err.clone())
        };
        if err.is_some() || path != self.paths.config {
            return false;
        }
        if !path.exists() {
            crate::logx::info(
                "Config file does not exist, treating as first start",
                &[("configPath", path.to_string_lossy().into())],
            );
            return true;
        }
        config::load(&path).is_err()
    }

    fn load_config(&self) -> Result<(), String> {
        let path = self.config_path();
        if path.as_os_str().is_empty() {
            return Err("no config file specified".into());
        }
        let cfg = config::load(&path).map_err(|e| e.to_string())?;
        lock(&self.state).config = Some(Arc::new(cfg));
        Ok(())
    }

    fn create_default_config(&self) -> Result<(), String> {
        let mut cfg = config::default_config();
        cfg.database.database_path = self.paths.database.to_string_lossy().into_owned();
        self.save_config_file(&cfg)
            .map_err(|e| format!("failed to save default config: {e}"))?;
        crate::logx::info(
            "Default config file created",
            &[("path", self.config_path().to_string_lossy().into())],
        );
        Ok(())
    }

    /// Loads or creates the config, then brings up the components (all skipped on first start).
    pub fn startup(self: &Arc<Self>) {
        crate::logx::info(
            "Application starting with OS-specific paths",
            &[
                ("config", self.paths.config.to_string_lossy().into()),
                ("database", self.paths.database.to_string_lossy().into()),
            ],
        );
        let first = self.determine_first_start();
        crate::logx::info(
            "First start determination",
            &[("isFirstStart", first.into())],
        );
        lock(&self.state).first_start = first;
        let (loc_err, path) = {
            let s = lock(&self.state);
            (s.config_location_err.clone(), s.config_path.clone())
        };
        if let Some(e) = loc_err {
            eprintln!("backend: cannot load saved configuration location: {e}");
        } else if let Err(e) = self.load_config() {
            if path == self.paths.config {
                crate::logx::info(
                    "Config file not found or invalid, creating default config",
                    &[
                        ("path", path.to_string_lossy().into()),
                        ("error", e.clone().into()),
                    ],
                );
                match self.create_default_config() {
                    Ok(()) => {
                        crate::logx::info(
                            "Default config created successfully",
                            &[("path", path.to_string_lossy().into())],
                        );
                        // on first start this is expected to fail validation (no servers yet)
                        match self.load_config() {
                            Ok(()) => crate::logx::info("Default config loaded successfully", &[]),
                            Err(e) => crate::logx::debug(
                                "Default config validation pending (expected on first start)",
                                &[("error", e.into())],
                            ),
                        }
                    }
                    Err(e) => {
                        crate::logx::error(
                            "Failed to create default config",
                            &[("error", e.into())],
                        );
                    }
                }
            } else {
                let msg = format!("Cannot load configuration at {}: {e}", path.display());
                eprintln!("backend: {msg}");
                lock(&self.state).config_load_error = msg;
            }
        }
        if let Some(cfg) = self.applied_config() {
            match Manager::new(&cfg) {
                Ok(m) => lock(&self.state).pool = Some(Arc::new(m)),
                Err(e) => {
                    lock(&self.state).critical_error =
                        format!("Failed to create connection pool: {e}")
                }
            }
        }
        let skip = lock(&self.state).first_start || self.applied_config().is_none();
        if skip {
            crate::logx::info(
                "Skipping component initialization: configuration required",
                &[],
            );
            return;
        }
        if let Err(e) = self.initialize_queue() {
            eprintln!("backend: failed to initialize queue: {e}");
        }
        if let Err(e) = self.initialize_processor() {
            lock(&self.state).critical_error.clone_from(&e);
            eprintln!("backend: failed to initialize processor: {e}");
        }
        self.initialize_post_check_worker();
        if let Err(e) = self.initialize_watchers() {
            eprintln!("backend: failed to initialize watchers: {e}");
        }
    }

    /// Stops everything in dependency order.
    pub async fn shutdown(&self) {
        eprintln!("backend: application shutdown initiated");
        let (post_check, watch_cancel, processor, pool, proc_cancel) = {
            let mut s = lock(&self.state);
            (
                s.post_check.take(),
                s.watch_cancel.take(),
                s.processor.take(),
                s.pool.clone(),
                s.proc_cancel.take(),
            )
        };
        if let Some(w) = post_check {
            w.stop();
        }
        if let Some(c) = watch_cancel {
            c.cancel();
        }
        lock(&self.state).watchers.clear();
        if let Some(p) = processor {
            p.close().await;
        }
        if let Some(c) = proc_cancel {
            c.cancel();
        }
        if let Some(p) = pool {
            p.close();
        }
        self.root.cancel();
        eprintln!("backend: application shutdown completed");
    }

    fn initialize_queue(&self) -> Result<(), String> {
        let cfg = self.applied_config().ok_or("config not loaded")?;
        let old_cancel = {
            let mut s = lock(&self.state);
            s.queue = None;
            s.db = None;
            s.proc_cancel.take()
        };
        if let Some(c) = old_cancel {
            c.cancel();
        }
        let cancel = self.root.child_token();
        let db = database::open(&cfg.database.database_type, &cfg.database.database_path)
            .map_err(|e| format!("failed to create database: {e}"))?;
        MigrationRunner::new(db.clone())
            .ensure_migration_compatibility()
            .map_err(|e| format!("failed to run database migrations: {e}"))?;
        let queue = Queue::new(db.clone());
        if cfg.api.enabled
            && let Err(e) = apikey::ensure_key(&SqlStore::new(db.clone()))
        {
            eprintln!("backend: failed to ensure API key: {e}");
        }
        let mut s = lock(&self.state);
        s.db = Some(db);
        s.queue = Some(queue);
        s.proc_cancel = Some(cancel);
        eprintln!("backend: queue initialized successfully");
        Ok(())
    }

    fn output_dir_abs(&self, cfg: &ConfigData) -> PathBuf {
        let out = cfg.output_dir();
        if out.is_empty() {
            return self.paths.data.join("output");
        }
        let p = Path::new(out);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            self.paths.data.join(p)
        }
    }

    fn initialize_processor(self: &Arc<Self>) -> Result<(), String> {
        let cfg = self.applied_config().ok_or("config not loaded")?;
        let old = lock(&self.state).processor.take();
        if let Some(p) = old {
            eprintln!("backend: closing existing processor before reinitialization");
            drop(tokio::spawn(async move { p.close().await }));
        }
        if valid_server_count(&cfg) == 0 {
            eprintln!("backend: no valid servers configured, skipping processor initialization");
            return Ok(());
        }
        let (pool, queue, cancel) = {
            let s = lock(&self.state);
            (s.pool.clone(), s.queue.clone(), s.proc_cancel.clone())
        };
        let (Some(pool), Some(queue), Some(cancel)) = (pool, queue, cancel) else {
            eprintln!(
                "backend: pool manager or queue not available, skipping processor initialization"
            );
            return Ok(());
        };
        let out = self.output_dir_abs(&cfg);
        std::fs::create_dir_all(&out)
            .map_err(|e| format!("failed to create output directory: {e}, {}", out.display()))?;
        let w = cfg.watcher_config();
        let me = Arc::downgrade(self);
        let (m2, m3) = (me.clone(), me.clone());
        let processor = Processor::new(ProcessorOptions {
            queue,
            config: cfg.clone(),
            queue_config: cfg.queue.clone(),
            pool: Some(pool as Arc<dyn PoolManager>),
            output_folder: out.to_string_lossy().into_owned(),
            delete_original_file: w.delete_original_file,
            delete_delay: Duration::from_nanos(
                u64::try_from(w.min_file_age_to_delete.nanos()).unwrap_or(0),
            ),
            maintain_original_extension: cfg.maintain_original_extension(),
            watch_folder: w.watch_directory.clone(),
            follow_symlinks: w.follow_symlinks,
            can_process_next_item: Some(Arc::new(move || {
                m2.upgrade().is_none_or(|a| a.can_process_next_item())
            })),
            on_job_error: Some(Arc::new(move |name, msg| {
                if let Some(a) = m3.upgrade() {
                    a.emit("job-error", Some(json!({"fileName": name, "error": msg})));
                }
            })),
            on_job_complete: Some(Arc::new(move || {
                if let Some(a) = me.upgrade() {
                    a.emit("queue-updated", None);
                }
            })),
        });
        let p2 = processor.clone();
        let token = cancel.clone();
        tokio::spawn(async move { p2.start(&token).await });
        lock(&self.state).processor = Some(processor);
        eprintln!(
            "backend: processor initialized successfully: outputDir={}",
            out.display()
        );
        Ok(())
    }

    /// Starts the legacy deferred-check worker (not in durable mode, where the verification service
    /// owns that work).
    fn initialize_post_check_worker(self: &Arc<Self>) {
        let old = lock(&self.state).post_check.take();
        if let Some(w) = old {
            w.stop();
        }
        let (cfg, queue, pool, processor) = {
            let s = lock(&self.state);
            (
                s.config.clone(),
                s.queue.clone(),
                s.pool.clone(),
                s.processor.clone(),
            )
        };
        let (Some(cfg), Some(queue), Some(pool)) = (cfg, queue, pool) else {
            return;
        };
        if cfg.post_check.enabled != Some(true) {
            return;
        }
        if processor.is_some_and(|p| p.durable_mode_enabled()) {
            eprintln!(
                "backend: durable verification active, skipping legacy post check retry worker"
            );
            return;
        }
        let Some(verify) = pool
            .verify_pool()
            .map(|c| c as Arc<dyn crate::poster::ArticleClient>)
        else {
            return;
        };
        let me = Arc::downgrade(self);
        let worker = PostCheckRetryWorker::new(
            &self.root,
            queue,
            Arc::new(PoolStater::new(verify, 16)),
            &cfg.post_check,
            Some(Arc::new(move || {
                if let Some(a) = me.upgrade() {
                    a.emit("queue-updated", None)
                }
            })),
        );
        let _ = worker.start();
        lock(&self.state).post_check = Some(worker);
    }

    fn initialize_watchers(self: &Arc<Self>) -> Result<(), String> {
        let cfg = self.applied_config().ok_or("config not loaded")?;
        let old = lock(&self.state).watch_cancel.take();
        if let Some(c) = old {
            c.cancel();
        }
        lock(&self.state).watchers.clear();
        let Some(queue) = self.queue() else {
            eprintln!("backend: queue not available, skipping watcher initialization");
            return Ok(());
        };
        let cancel = self.root.child_token();
        let processor = self.processor();
        let mut started = Vec::new();
        for wc in cfg.watchers.iter().flatten() {
            if !wc.enabled {
                continue;
            }
            let dir = if wc.watch_directory.is_empty() {
                self.paths.data.join("watch")
            } else {
                PathBuf::from(&wc.watch_directory)
            };
            if let Err(e) = std::fs::create_dir_all(&dir) {
                eprintln!(
                    "backend: failed to create watch directory: {} ({e})",
                    dir.display()
                );
                continue;
            }
            let me = Arc::downgrade(self);
            let mut w = Watcher::new(
                wc.clone(),
                Arc::new(queue.clone()),
                processor
                    .clone()
                    .map(|p| p as Arc<dyn crate::watcher::ProcessorInterface>),
                &dir.to_string_lossy(),
            );
            w = w.with_par2_config(Arc::new(move || {
                me.upgrade()
                    .and_then(|a| a.applied_config())
                    .map(|c| c.par2.clone())
            }));
            let w = Arc::new(w);
            tokio::spawn(w.clone().start(cancel.clone()));
            started.push(w);
        }
        let mut s = lock(&self.state);
        s.watchers = started;
        s.watch_cancel = Some(cancel);
        Ok(())
    }

    pub fn is_uploading(&self) -> bool {
        self.processor()
            .is_some_and(|p| !p.get_running_jobs().is_empty())
    }

    fn has_active_work(&self) -> bool {
        self.is_uploading()
    }

    // ---- config ----

    /// The pending config when one exists, else the applied one (loading from disk when needed).
    pub fn get_config(&self) -> Result<ConfigData, String> {
        let (path, err, pending, applied) = {
            let s = lock(&self.state);
            (
                s.config_path.clone(),
                s.config_location_err.clone(),
                s.pending.clone(),
                s.config.clone(),
            )
        };
        if let Some(e) = err {
            return Err(e);
        }
        if path.as_os_str().is_empty() {
            return Err("no config file specified".into());
        }
        if !path.exists() {
            return Err("config file not found".into());
        }
        let result = if let Some(p) = pending {
            p
        } else if let Some(a) = applied {
            (*a).clone()
        } else {
            config::load(&path).map_err(|e| format!("error loading config file: {e}"))?
        };
        lock(&self.state).config_load_error.clear();
        Ok(result)
    }

    /// The applied config, ignoring pending changes.
    pub fn get_applied_config(&self) -> Result<ConfigData, String> {
        let path = self.config_path();
        if path.as_os_str().is_empty() {
            return Err("no config file specified".into());
        }
        if !path.exists() {
            return Err("config file not found".into());
        }
        match self.applied_config() {
            Some(a) => Ok((*a).clone()),
            None => config::load(&path).map_err(|e| format!("error loading config file: {e}")),
        }
    }

    /// Checks every configured server can be reached; the message lists the failures.
    pub async fn validate_server_connections(&self, cfg: &ConfigData) -> Result<(), String> {
        let servers: Vec<&ServerConfig> = cfg.servers.iter().flatten().collect();
        if servers.iter().all(|s| s.host.is_empty()) {
            return Ok(());
        }
        let mut failed = Vec::new();
        let mut last_error = String::new();
        for (i, s) in servers.iter().enumerate() {
            if s.host.is_empty() {
                continue;
            }
            let data = ServerData {
                host: s.host.clone(),
                port: s.port,
                username: s.username.clone(),
                password: s.password.clone(),
                ssl: s.ssl,
                max_connections: s.max_connections,
                ..Default::default()
            };
            let res = tokio::time::timeout(Duration::from_secs(30), self.tester.test(&data)).await;
            let err = match res {
                Ok(r) if r.valid => continue,
                Ok(r) => format!("server {} ({}:{}): {}", i + 1, s.host, s.port, r.error),
                Err(_) => format!(
                    "server {} ({}:{}): connection timeout after 30 seconds",
                    i + 1,
                    s.host,
                    s.port
                ),
            };
            failed.push(format!("{}:{}", s.host, s.port));
            last_error = err;
        }
        if failed.is_empty() {
            return Ok(());
        }
        let mut msg = format!(
            "Failed to connect to {} server(s): {}",
            failed.len(),
            failed.join(", ")
        );
        if !last_error.is_empty() {
            msg = format!("{msg}. Last error: {last_error}");
        }
        Err(msg)
    }

    /// Validates and saves; applies at once, or stores it as pending while uploads are running.
    pub async fn save_config(self: &Arc<Self>, mut cfg: ConfigData) -> Result<(), String> {
        cfg.validate()?;
        cfg.version = config::CURRENT_CONFIG_VERSION;
        self.validate_server_connections(&cfg).await?;
        if self.has_active_work() {
            lock(&self.state).pending = Some(cfg.clone());
            self.save_config_file(&cfg)?;
            self.emit("config-pending", Some(json!({"hasPendingConfig": true, "message": "Configuration changes will be applied when current uploads finish"})));
            return Ok(());
        }
        self.apply_config_changes(&cfg)
    }

    fn apply_config_changes(self: &Arc<Self>, cfg: &ConfigData) -> Result<(), String> {
        self.save_config_file(cfg)?;
        let old = self.applied_config();
        let (pool_changed, proc_restart, watch_restart) = (
            pool_config_changed(old.as_deref(), cfg),
            processor_config_changed(old.as_deref(), cfg),
            watcher_config_changed(old.as_deref(), cfg),
        );
        self.load_config()?;
        lock(&self.state).pending = None;
        if self.queue().is_none()
            && let Err(e) = self.initialize_queue()
        {
            eprintln!("backend: failed to initialize queue after config change: {e}");
        }
        let applied = self.applied_config().ok_or("config not loaded")?;
        let mut pool_created = false;
        let pool = self.pool_manager();
        match pool {
            Some(p) if pool_changed => {
                if let Err(e) = p.update_config(&applied) {
                    eprintln!(
                        "backend: failed to update connection pool manager with new config: {e}"
                    );
                }
            }
            Some(_) => {}
            None if applied.servers.as_ref().is_some_and(|s| !s.is_empty()) => {
                match Manager::new(&applied) {
                    Ok(m) => {
                        lock(&self.state).pool = Some(Arc::new(m));
                        pool_created = true;
                    }
                    Err(e) => eprintln!("backend: failed to create connection pool manager: {e}"),
                }
            }
            None => {}
        }
        if proc_restart || pool_created {
            if let Err(e) = self.initialize_processor() {
                eprintln!("backend: failed to re-initialize processor after config change: {e}");
            }
            self.initialize_post_check_worker();
        }
        if watch_restart && let Err(e) = self.initialize_watchers() {
            eprintln!("backend: failed to re-initialize watchers after config change: {e}");
        }
        self.emit("config-updated", serde_json::to_value(cfg).ok());
        eprintln!("backend: config applied successfully");
        Ok(())
    }

    pub fn has_pending_config_changes(&self) -> bool {
        lock(&self.state).pending.is_some()
    }

    pub fn pending_config_status(&self) -> Value {
        let pending = lock(&self.state).pending.is_some();
        let mut v = json!({"hasPendingConfig": pending, "canApplyNow": !self.has_active_work()});
        if pending {
            v["message"] = json!(
                "Configuration changes are pending. They will be applied when uploads finish."
            );
        }
        v
    }

    pub fn get_pending_config(&self) -> Option<ConfigData> {
        lock(&self.state).pending.clone()
    }

    pub fn apply_pending_config(self: &Arc<Self>) -> Result<(), String> {
        let pending = lock(&self.state)
            .pending
            .clone()
            .ok_or("no pending configuration changes")?;
        if self.has_active_work() {
            return Err("cannot apply configuration while uploads or jobs are running".into());
        }
        self.apply_config_changes(&pending)
    }

    pub fn discard_pending_config(&self) -> Result<(), String> {
        let mut s = lock(&self.state);
        if s.pending.take().is_none() {
            return Err("no pending configuration changes to discard".into());
        }
        drop(s);
        self.emit("config-pending", Some(json!({"hasPendingConfig": false, "message": "Pending configuration changes have been discarded"})));
        Ok(())
    }

    /// False while pending config blocks new items; when idle it applies the pending config in the
    /// background (one attempt at a time).
    pub fn can_process_next_item(self: &Arc<Self>) -> bool {
        if lock(&self.state).pending.is_none() {
            return true;
        }
        if !self.has_active_work()
            && self
                .applying
                .compare_exchange(
                    false,
                    true,
                    std::sync::atomic::Ordering::SeqCst,
                    std::sync::atomic::Ordering::SeqCst,
                )
                .is_ok()
        {
            eprintln!(
                "backend: no active work detected, attempting to apply pending configuration"
            );
            let me = self.clone();
            tokio::task::spawn_blocking(move || {
                if let Err(e) = me.apply_pending_config() {
                    eprintln!("backend: failed to auto-apply pending configuration: {e}");
                }
                me.applying
                    .store(false, std::sync::atomic::Ordering::SeqCst);
            });
        }
        false
    }

    // ---- status ----

    pub fn get_app_status(&self) -> AppStatus {
        let (cfg, config_error, critical, first, path) = {
            let s = lock(&self.state);
            (
                s.config.clone(),
                s.config_location_err
                    .clone()
                    .unwrap_or_else(|| s.config_load_error.clone()),
                s.critical_error.clone(),
                s.first_start,
                s.config_path.clone(),
            )
        };
        let error = if critical.is_empty() {
            config_error
        } else {
            critical
        };
        let (has_servers, count, valid) = match &cfg {
            Some(c) => (
                c.servers.as_ref().is_some_and(|s| !s.is_empty()),
                c.servers.as_ref().map_or(0, Vec::len),
                valid_server_count(c),
            ),
            None => (false, 0, 0),
        };
        AppStatus {
            has_config: cfg.is_some(),
            config_path: path.to_string_lossy().into_owned(),
            uploading: self.is_uploading(),
            critical_config_error: !error.is_empty(),
            error,
            is_first_start: first,
            has_servers,
            server_count: count,
            valid_server_count: valid,
            config_valid: has_servers && valid > 0,
            needs_configuration: !has_servers || valid == 0,
            version: std::env::var("POSTIE_VERSION")
                .ok()
                .filter(|v| !v.is_empty())
                .unwrap_or_else(|| option_env!("POSTIE_VERSION").unwrap_or("dev").to_string()),
        }
    }

    pub fn get_processor_status(&self) -> ProcessorStatus {
        match self.processor() {
            Some(p) => {
                let ids: Vec<String> = p.get_running_jobs().into_keys().collect();
                ProcessorStatus {
                    has_processor: true,
                    running_jobs: ids.len(),
                    running_job_ids: ids,
                }
            }
            None => ProcessorStatus {
                has_processor: false,
                running_jobs: 0,
                running_job_ids: vec![],
            },
        }
    }

    pub fn get_transfer_runtime_metrics(&self) -> RuntimeMetrics {
        self.processor()
            .map(|p| p.transfer_runtime_metrics())
            .unwrap_or_default()
    }

    pub fn get_running_jobs(&self) -> Vec<RunningJobItem> {
        self.processor()
            .map(|p| p.get_running_job_items())
            .unwrap_or_default()
    }

    pub fn get_running_jobs_details(&self) -> Vec<RunningJobDetails> {
        self.processor()
            .map(|p| p.get_running_job_details().into_values().collect())
            .unwrap_or_default()
    }

    pub fn get_watch_directory(&self) -> String {
        let from_cfg = self
            .applied_config()
            .map(|c| c.watcher_config().watch_directory)
            .filter(|d| !d.is_empty());
        from_cfg.unwrap_or_else(|| self.paths.data.join("watch").to_string_lossy().into_owned())
    }

    pub fn get_output_directory(&self) -> String {
        match self.applied_config() {
            Some(c) if !c.output_dir().is_empty() => {
                self.output_dir_abs(&c).to_string_lossy().into_owned()
            }
            _ => self
                .paths
                .data
                .join("output")
                .to_string_lossy()
                .into_owned(),
        }
    }

    // ---- processor and queue actions ----

    pub fn pause_processing(&self) -> Result<(), String> {
        let p = self.processor().ok_or("processor not initialized")?;
        p.pause_processing();
        self.emit("processing:paused", Some(self.pause_state()));
        Ok(())
    }

    pub fn resume_processing(&self) -> Result<(), String> {
        let p = self.processor().ok_or("processor not initialized")?;
        p.resume_processing();
        self.emit("processing:resumed", Some(self.pause_state()));
        Ok(())
    }

    pub fn is_processing_paused(&self) -> bool {
        self.processor().is_some_and(|p| p.is_paused())
    }

    pub fn is_processing_auto_paused(&self) -> bool {
        self.processor().is_some_and(|p| p.is_auto_paused())
    }

    pub fn auto_pause_reason(&self) -> String {
        self.processor()
            .map(|p| p.auto_pause_reason())
            .unwrap_or_default()
    }

    pub fn pause_state(&self) -> Value {
        let p = self.processor();
        json!({
            "paused": p.as_ref().is_some_and(|p| p.is_paused()),
            "autoPaused": p.as_ref().is_some_and(|p| p.is_auto_paused()),
            "reason": p.as_ref().map(|p| p.auto_pause_reason()).unwrap_or_default(),
        })
    }

    pub fn cancel_job(&self, id: &str) -> Result<(), String> {
        self.processor()
            .ok_or_else(|| "processor not available".to_string())?
            .cancel_job(id)
    }

    pub fn retry_job(&self, id: &str) -> Result<(), String> {
        let Some(q) = self.queue() else { return Ok(()) };
        q.retry_errored_job(id).map_err(|e| e.to_string())?;
        self.emit("queue:updated", None);
        Ok(())
    }

    pub fn retry_script(&self, id: &str) -> Result<(), String> {
        let q = self.queue().ok_or("queue is not available")?;
        q.update_script_status(id, "pending_retry", 0, "", Some(chrono::Utc::now()), None)
            .map_err(|e| format!("failed to reset script status: {e}"))?;
        self.emit("queue:updated", None);
        Ok(())
    }

    pub async fn test_provider_connectivity(&self, s: ServerData) -> ValidationResult {
        self.tester.test(&s).await
    }

    // ---- watchers ----

    pub fn trigger_scan(&self) {
        let (ws, cancel) = {
            let s = lock(&self.state);
            (s.watchers.clone(), s.watch_cancel.clone())
        };
        if ws.is_empty() {
            eprintln!("backend: TriggerScan called but no watchers are initialized");
            return;
        }
        let cancel = cancel.unwrap_or_else(|| self.root.clone());
        for w in ws {
            w.trigger_scan(&cancel);
        }
    }

    pub fn get_watcher_status(&self) -> Vec<WatcherStatusInfo> {
        let Some(cfg) = self.applied_config() else {
            return vec![];
        };
        let running: std::collections::HashMap<String, Arc<Watcher>> = lock(&self.state)
            .watchers
            .iter()
            .map(|w| (w.watcher_status().watch_directory, w.clone()))
            .collect();
        cfg.watchers
            .iter()
            .flatten()
            .map(|wc| self.one_watcher_status(wc, &running))
            .collect()
    }

    fn one_watcher_status(
        &self,
        wc: &WatcherConfig,
        running: &std::collections::HashMap<String, Arc<Watcher>>,
    ) -> WatcherStatusInfo {
        if !wc.enabled {
            return WatcherStatusInfo {
                name: wc.name.clone(),
                enabled: false,
                initialized: false,
                watch_directory: wc.watch_directory.clone(),
                check_interval: wc.check_interval.0.clone(),
                next_run: String::new(),
                is_within_schedule: false,
                schedule: None,
                error: String::new(),
            };
        }
        let dir = if wc.watch_directory.is_empty() {
            self.paths.data.join("watch").to_string_lossy().into_owned()
        } else {
            wc.watch_directory.clone()
        };
        if let Some(w) = running.get(&dir) {
            let mut s = w.watcher_status();
            if s.name.is_empty() {
                s.name.clone_from(&wc.name);
            }
            return s;
        }
        let scheduled = !wc.schedule.start_time.is_empty() && !wc.schedule.end_time.is_empty();
        WatcherStatusInfo {
            name: wc.name.clone(),
            enabled: true,
            initialized: false,
            watch_directory: dir,
            check_interval: wc.check_interval.0.clone(),
            next_run: String::new(),
            is_within_schedule: true,
            schedule: scheduled.then(|| WatcherScheduleInfo {
                start_time: wc.schedule.start_time.clone(),
                end_time: wc.schedule.end_time.clone(),
            }),
            error: String::new(),
        }
    }
}

#[cfg(test)]
mod tests;
