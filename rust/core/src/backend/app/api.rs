//! The queue, log, upload-intake, setup and metrics operations of the application service
//! (`backend/queue.go`, `app.go`, `utils.go`).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{App, ServerData, lock};
use crate::config::{self, ConfigData, ServerConfig};
use crate::queue::{AddOptions, PaginationParams};
use crate::transferstore::fmt_time;

/// The queue item as the API returns it: the queue's own item minus the script fields.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    pub id: String,
    pub path: String,
    pub file_name: String,
    pub size: i64,
    pub status: String,
    pub retry_count: i64,
    pub priority: i64,
    pub error_message: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub completed_at: Option<String>,
    pub nzb_path: Option<String>,
    pub verification_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified_articles: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_articles: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaginatedQueueResult {
    pub items: Vec<QueueItem>,
    pub total_items: i64,
    pub total_pages: i64,
    pub current_page: i64,
    pub items_per_page: i64,
    pub has_next: bool,
    pub has_prev: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueStats {
    pub total: i64,
    pub pending: i64,
    pub running: i64,
    pub complete: i64,
    pub error: i64,
    pub pending_verification: i64,
    pub verification_failed: i64,
}

pub const QUEUE_STATS_CACHE_TTL: Duration = Duration::from_secs(2);

#[derive(Default)]
pub(super) struct StatsCache(pub Option<(Instant, QueueStats)>);

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct SetupWizardData {
    pub servers: Vec<ServerData>,
    #[serde(rename = "outputDirectory")]
    pub output_directory: String,
    #[serde(rename = "watchDirectory")]
    pub watch_directory: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NntpProviderMetrics {
    pub name: String,
    pub host: String,
    pub active_connections: i64,
    pub max_connections: i64,
    pub available_slots: i64,
    pub total_errors: i64,
    /// Article bytes this provider accepted (`240`) since the pool started.
    pub bytes_uploaded: i64,
    pub articles_posted: i64,
    /// `STAT` answers of `430` (verification found the article missing).
    pub missing: i64,
    pub inflight: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NntpPoolMetrics {
    pub timestamp: String,
    pub active_connections: i64,
    pub total_errors: i64,
    pub bytes_uploaded: i64,
    pub articles_posted: i64,
    #[serde(serialize_with = "crate::gofloat::serialize")]
    pub upload_speed: f64,
    #[serde(serialize_with = "crate::gofloat::serialize")]
    pub upload_avg_speed: f64,
    pub provider_errors: BTreeMap<String, i64>,
    pub providers: Vec<NntpProviderMetrics>,
}

fn rfc3339_secs(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Lines of `content` counted back from the end: `limit` lines after skipping `offset` newest ones.
pub fn read_log_lines(content: &str, limit: i64, offset: i64) -> Result<String, String> {
    if content.is_empty() {
        return Ok(String::new());
    }
    let mut lines: Vec<&str> = content.split('\n').collect();
    if lines.last() == Some(&"") {
        lines.pop();
    }
    let total = lines.len() as i64;
    if total == 0 {
        return Ok(String::new());
    }
    let offset = offset.max(0);
    if limit <= 0 {
        return Err("limit must be positive".into());
    }
    let end = total - offset;
    if end <= 0 {
        return Ok(String::new());
    }
    let start = (end - limit).max(0);
    if start >= total {
        return Ok(String::new());
    }
    Ok(lines[start as usize..end.min(total) as usize].join("\n"))
}

/// Collects a directory's files grouped by folder with sizes; unreadable entries are skipped.
pub fn process_directory_recursively(
    dir: &Path,
) -> (BTreeMap<String, Vec<String>>, BTreeMap<String, i64>) {
    let (mut files, mut sizes) = (BTreeMap::new(), BTreeMap::new());
    fn walk(
        dir: &Path,
        files: &mut BTreeMap<String, Vec<String>>,
        sizes: &mut BTreeMap<String, i64>,
    ) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for e in entries {
            let Ok(meta) = std::fs::symlink_metadata(e.path()) else {
                continue;
            };
            if meta.is_dir() {
                walk(&e.path(), files, sizes);
                continue;
            }
            let folder = dir.to_string_lossy().into_owned();
            files
                .entry(folder.clone())
                .or_default()
                .push(e.path().to_string_lossy().into_owned());
            *sizes.entry(folder).or_default() += i64::try_from(meta.len()).unwrap_or(i64::MAX);
        }
    }
    walk(dir, &mut files, &mut sizes);
    (files, sizes)
}

impl App {
    pub fn get_queue_items(
        &self,
        params: PaginationParams,
    ) -> Result<PaginatedQueueResult, String> {
        let Some(q) = self.queue() else {
            return Ok(PaginatedQueueResult {
                items: vec![],
                total_items: 0,
                total_pages: 0,
                current_page: params.page,
                items_per_page: params.limit,
                has_next: false,
                has_prev: false,
            });
        };
        let r = q.get_queue_items(params).map_err(|e| e.to_string())?;
        let items = r
            .items
            .into_iter()
            .map(|i| QueueItem {
                id: i.id,
                path: i.path,
                file_name: i.file_name,
                size: i.size,
                status: i.status,
                retry_count: i.retry_count,
                priority: i.priority,
                error_message: i.error_message,
                created_at: fmt_time(i.created_at),
                updated_at: fmt_time(i.updated_at),
                completed_at: i.completed_at.map(fmt_time),
                nzb_path: i.nzb_path,
                verification_status: i.verification_status,
                verified_articles: i.verified_articles,
                total_articles: i.total_articles,
            })
            .collect();
        Ok(PaginatedQueueResult {
            items,
            total_items: r.total_items,
            total_pages: r.total_pages,
            current_page: r.current_page,
            items_per_page: r.items_per_page,
            has_next: r.has_next,
            has_prev: r.has_prev,
        })
    }

    /// Removes an item; a running job is stopped first (it would otherwise keep uploading and re-create a
    /// completed row).
    pub fn remove_from_queue(&self, id: &str) -> Result<(), String> {
        let q = self.queue().ok_or("queue not initialized")?;
        if let Some(p) = self.processor()
            && p.get_running_jobs().contains_key(id)
            && let Err(e) = p.cancel_job(id)
        {
            crate::logx::warn(
                &format!("Failed to cancel running job before removal: id={id} error={e}"),
                &[("component", "backend".into())],
            );
        }
        q.remove_from_queue(id).map_err(|e| e.to_string())?;
        self.emit("queue-updated", None);
        Ok(())
    }

    pub fn clear_queue(&self) -> Result<(), String> {
        let q = self.queue().ok_or("queue not initialized")?;
        q.clear_queue().map_err(|e| e.to_string())?;
        self.emit("queue-updated", None);
        Ok(())
    }

    /// Counts per state, cached for 2 s so dashboard polling cannot pile up behind upload writes.
    pub fn get_queue_stats(&self) -> Result<QueueStats, String> {
        let Some(q) = self.queue() else {
            return Ok(QueueStats::default());
        };
        let mut cache = lock(&self.stats_cache);
        if let Some((at, s)) = cache.0
            && at.elapsed() < QUEUE_STATS_CACHE_TTL
        {
            return Ok(s);
        }
        let m = q.get_queue_stats().map_err(|e| e.to_string())?;
        let n = |k: &str| m.get(k).and_then(Value::as_i64).unwrap_or(0);
        // the dashboard's "Total Items" counts completed ones too, unlike the live backlog `total`
        let s = QueueStats {
            total: n("total_including_completed"),
            pending: n("pending"),
            running: n("running"),
            complete: n("complete"),
            error: n("error"),
            pending_verification: n("pendingVerification"),
            verification_failed: n("verificationFailed"),
        };
        cache.0 = Some((Instant::now(), s));
        Ok(s)
    }

    /// The NZB's content and file name for a completed item.
    pub fn get_nzb(&self, id: &str) -> Result<(Vec<u8>, String), String> {
        let q = self.queue().ok_or("queue not initialized")?;
        let path = q
            .get_completed_item_nzb_path(id)
            .map_err(|e| format!("failed to get NZB path: {e}"))?;
        if !Path::new(&path).exists() {
            return Err(format!("NZB file not found: {path}"));
        }
        let data = std::fs::read(&path).map_err(|e| format!("failed to read NZB file: {e}"))?;
        let name = Path::new(&path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Ok((data, name))
    }

    pub fn set_queue_item_priority(&self, id: &str, priority: i64) -> Result<(), String> {
        let q = self.queue().ok_or("queue not initialized")?;
        q.set_queue_item_priority_with_reorder(id, priority)
            .map_err(|e| e.to_string())?;
        self.emit("queue-updated", None);
        Ok(())
    }

    pub fn debug_queue_item(&self, id: &str) -> Result<serde_json::Map<String, Value>, String> {
        self.queue()
            .ok_or("queue not initialized")?
            .debug_queue_item(id)
            .map_err(|e| e.to_string())
    }

    /// Queues dropped/uploaded/imported paths: files individually, directories as one `FOLDER:` job.
    pub fn handle_dropped_files(&self, paths: &[String]) -> Result<(), String> {
        self.enqueue_dropped(paths, false)
    }

    /// Like [`Self::handle_dropped_files`] for Postie's own temporary copies of web uploads: the
    /// copies are deleted once uploaded (after verification in durable mode), so they never pile up.
    pub fn handle_uploaded_copies(&self, paths: &[String]) -> Result<(), String> {
        self.enqueue_dropped(paths, true)
    }

    fn enqueue_dropped(&self, paths: &[String], delete_after: bool) -> Result<(), String> {
        let options = || AddOptions {
            delete_original: Some(delete_after),
            ..Default::default()
        };
        if paths.is_empty() {
            return Err("no files dropped".into());
        }
        crate::logx::info(
            &format!("Files dropped onto window: count={}", paths.len()),
            &[("component", "backend".into())],
        );
        if self.get_app_status().needs_configuration {
            return Err("configuration required: Please configure at least one server in the Settings page before uploading files".into());
        }
        let q = self.queue().ok_or("queue not initialized")?;
        let (mut added, mut active) = (0, 0);
        for p in paths {
            let Ok(meta) = std::fs::metadata(p) else {
                crate::logx::warn(
                    &format!("Could not get file info for dropped file, skipping: {p}"),
                    &[("component", "backend".into())],
                );
                continue;
            };
            if meta.is_dir() {
                let (by_folder, sizes) = process_directory_recursively(Path::new(p));
                let total_files: usize = by_folder.values().map(Vec::len).sum();
                if total_files == 0 {
                    crate::logx::info(
                        &format!("Directory contains no files, skipping: {p}"),
                        &[("component", "backend".into())],
                    );
                    continue;
                }
                let total: i64 = sizes.values().sum();
                match q.add_user_file(&format!("FOLDER:{p}"), total, options()) {
                    Ok(true) => added += 1,
                    Ok(false) => active += 1,
                    Err(e) => crate::logx::warn(
                        &format!("Could not add folder to queue, skipping: {p} ({e})"),
                        &[("component", "backend".into())],
                    ),
                }
                continue;
            }
            match q.add_user_file(p, i64::try_from(meta.len()).unwrap_or(i64::MAX), options()) {
                Ok(true) => added += 1,
                Ok(false) => active += 1,
                Err(e) => crate::logx::warn(
                    &format!("Could not add dropped file to queue, skipping: {p} ({e})"),
                    &[("component", "backend".into())],
                ),
            }
        }
        if added > 0 {
            self.emit("queue-updated", None);
        }
        if added == 0 && active > 0 {
            return Err(
                "already in the queue: these files are waiting or uploading right now".into(),
            );
        }
        if added == 0 {
            return Err("no valid files or folders could be added to queue".into());
        }
        Ok(())
    }

    /// Adds a file through the gated HTTP API with per-job options.
    pub fn enqueue_api_upload(
        &self,
        file: &str,
        relative_path: &str,
        priority: i64,
        delete_after: bool,
    ) -> Result<Value, String> {
        let q = self.queue().ok_or("queue not initialized")?;
        if file.is_empty() {
            return Err("file is required".into());
        }
        if relative_path.is_empty() {
            return Err("relative_path is required".into());
        }
        let clean_file = clean_path(file);
        let clean_root = clean_path(relative_path);
        if !Path::new(&clean_file).is_absolute() {
            return Err(format!("file must be an absolute path: {file:?}"));
        }
        if !Path::new(&clean_root).is_absolute() {
            return Err(format!(
                "relative_path must be an absolute path: {relative_path:?}"
            ));
        }
        if !crate::postie::paths::is_strictly_inside(&clean_root, &clean_file) {
            return Err(format!(
                "relative_path {relative_path:?} is not a parent directory of file {file:?}"
            ));
        }
        let meta = std::fs::metadata(&clean_file).map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("file not found: {clean_file}")
            } else {
                format!("stat file: {e}")
            }
        })?;
        if !meta.is_file() {
            return Err(format!("file must be a regular file: {clean_file}"));
        }
        q.add_file_with_options(
            &clean_file,
            i64::try_from(meta.len()).unwrap_or(i64::MAX),
            AddOptions {
                priority,
                input_folder: clean_root,
                delete_original: Some(delete_after),
            },
        )
        .map_err(|e| format!("enqueue file: {e}"))?;
        Ok(json!({"status": "queued", "file": clean_file}))
    }

    pub fn is_api_enabled(&self) -> bool {
        self.applied_config().is_some_and(|c| c.api.enabled)
    }

    pub fn get_api_key(&self) -> Result<String, String> {
        let db = self
            .db()
            .ok_or("api key store not initialized: database is unavailable")?;
        crate::apikey::ensure_key(&crate::apikey::SqlStore::new(db)).map_err(|e| e.to_string())
    }

    pub fn regenerate_api_key(&self) -> Result<String, String> {
        let db = self
            .db()
            .ok_or("api key store not initialized: database is unavailable")?;
        crate::apikey::regenerate(&crate::apikey::SqlStore::new(db)).map_err(|e| e.to_string())
    }

    // ---- logs ----

    fn log_path(&self) -> std::path::PathBuf {
        let actual = crate::logx::current_path();
        if actual.as_os_str().is_empty() {
            self.paths.log.clone()
        } else {
            actual
        }
    }

    /// `limit == 0`: the last 1 MiB; otherwise `limit` lines after skipping `offset` from the end.
    pub fn get_logs_paginated(&self, limit: i64, offset: i64) -> Result<String, String> {
        let path = self.log_path();
        let mut data = std::fs::read(&path)
            .map_err(|e| format!("failed to open log file at {}: {e}", path.display()))?;
        if limit == 0 {
            const MAX: usize = 1024 * 1024;
            if data.len() > MAX {
                data.drain(..data.len() - MAX);
            }
            return Ok(String::from_utf8_lossy(&data).into_owned());
        }
        read_log_lines(&String::from_utf8_lossy(&data), limit, offset)
    }

    pub fn get_logs(&self) -> Result<String, String> {
        self.get_logs_paginated(0, 0)
    }

    // ---- setup and validation ----

    pub async fn validate_nntp_server(&self, s: ServerData) -> super::ValidationResult {
        self.test_provider_connectivity(s).await
    }

    fn wrap_save_config_error(&self, e: &str) -> String {
        let path = self.config_path();
        if e.contains("permission denied") {
            return format!(
                "permission denied: cannot write configuration to '{}'. Please check directory permissions",
                path.display()
            );
        }
        if e.contains("no space left") {
            return "not enough disk space to save configuration".into();
        }
        if e.contains("server validation failed") || e.starts_with("Failed to connect to") {
            return e.to_string();
        }
        format!("failed to save configuration to '{}': {e}", path.display())
    }

    /// Builds a fresh config from the wizard answers and saves it.
    pub async fn setup_wizard_complete(
        self: &std::sync::Arc<Self>,
        wizard: SetupWizardData,
    ) -> Result<(), String> {
        if wizard.servers.is_empty() {
            return Err("at least one server must be configured".into());
        }
        if wizard.output_directory.is_empty() {
            return Err("output directory must be specified".into());
        }
        if wizard
            .servers
            .iter()
            .all(|s| s.role == config::SERVER_ROLE_VERIFY)
        {
            return Err("at least one upload server is required".into());
        }
        for (i, s) in wizard.servers.iter().enumerate() {
            if s.host.is_empty() {
                return Err(format!("server {}: host is required", i + 1));
            }
            if s.port <= 0 || s.port > 65535 {
                return Err(format!(
                    "server {}: port must be between 1 and 65535",
                    i + 1
                ));
            }
        }
        let mut cfg: ConfigData = config::default_config();
        cfg.version = config::CURRENT_CONFIG_VERSION;
        cfg.servers = Some(
            wizard
                .servers
                .iter()
                .map(|s| ServerConfig {
                    name: s.name.clone(),
                    host: s.host.clone(),
                    port: s.port,
                    username: s.username.clone(),
                    password: s.password.clone(),
                    ssl: s.ssl,
                    max_connections: if s.max_connections <= 0 {
                        5
                    } else {
                        s.max_connections
                    },
                    enabled: Some(true),
                    role: if s.role == config::SERVER_ROLE_VERIFY {
                        config::SERVER_ROLE_VERIFY.into()
                    } else {
                        config::SERVER_ROLE_UPLOAD.into()
                    },
                    ..Default::default()
                })
                .collect(),
        );
        cfg.output_dir = wizard.output_directory;
        cfg.database.database_path = self.paths.database.to_string_lossy().into_owned();
        self.save_config(cfg)
            .await
            .map_err(|e| self.wrap_save_config_error(&e))?;
        lock(&self.state).first_start = false;
        Ok(())
    }

    // ---- metrics ----

    pub fn get_nntp_pool_metrics(&self) -> Result<NntpPoolMetrics, String> {
        let now = rfc3339_secs(Utc::now());
        let empty = NntpPoolMetrics {
            timestamp: now.clone(),
            active_connections: 0,
            total_errors: 0,
            bytes_uploaded: 0,
            articles_posted: 0,
            upload_speed: 0.0,
            upload_avg_speed: 0.0,
            provider_errors: BTreeMap::new(),
            providers: vec![],
        };
        let Some(pool) = self.pool_manager() else {
            return Ok(empty);
        };
        let stats = pool
            .metrics()
            .map_err(|e| format!("failed to get pool metrics: {e}"))?;
        let up = self.get_transfer_runtime_metrics();
        let cfg = self.applied_config();
        let servers: Vec<&ServerConfig> = cfg
            .as_ref()
            .map(|c| c.servers.iter().flatten().collect())
            .unwrap_or_default();
        let key = |s: &ServerConfig| {
            if s.username.is_empty() {
                format!("{}:{}", s.host, s.port)
            } else {
                format!("{}:{}+{}", s.host, s.port, s.username)
            }
        };
        let mut provider_errors = BTreeMap::new();
        let providers: Vec<NntpProviderMetrics> = stats
            .iter()
            .map(|p| {
                provider_errors.insert(p.name.clone(), p.errors as i64);
                let srv = servers.iter().find(|s| key(s) == p.name);
                let max_connections = srv.map_or(0, |s| s.provider_params().connections);
                NntpProviderMetrics {
                    name: srv.map(|s| s.name.clone()).unwrap_or_default(),
                    host: p.name.clone(),
                    active_connections: p.active_connections as i64,
                    max_connections,
                    available_slots: available_slots(max_connections, p.active_connections),
                    total_errors: p.errors as i64,
                    bytes_uploaded: to_i64(p.posted_bytes),
                    articles_posted: to_i64(p.posted),
                    missing: p.stat_missing as i64,
                    inflight: srv.map_or(0, |s| s.provider_params().inflight),
                }
            })
            .collect();
        Ok(NntpPoolMetrics {
            timestamp: now,
            active_connections: stats.iter().map(|p| p.active_connections as i64).sum(),
            total_errors: stats.iter().map(|p| p.errors as i64).sum(),
            bytes_uploaded: up.upload_bytes,
            articles_posted: stats.iter().map(|p| to_i64(p.posted)).sum(),
            upload_speed: up.upload_speed_bps,
            upload_avg_speed: up.upload_avg_speed_bps,
            provider_errors,
            providers,
        })
    }
}

fn to_i64(v: u64) -> i64 {
    i64::try_from(v).unwrap_or(i64::MAX)
}

/// Connections a provider can still open: the configured limit minus the live ones, never negative
/// (an unknown provider has a limit of 0).
fn available_slots(max_connections: i64, active_connections: usize) -> i64 {
    let active = i64::try_from(active_connections).unwrap_or(i64::MAX);
    max_connections.saturating_sub(active).max(0)
}

pub use crate::postie::paths::clean_path;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_slots_is_the_unused_part_of_the_connection_limit() {
        assert_eq!(available_slots(50, 21), 29);
        assert_eq!(available_slots(50, 0), 50);
        assert_eq!(available_slots(50, 50), 0);
        assert_eq!(available_slots(10, 12), 0, "never negative");
        assert_eq!(available_slots(0, 3), 0, "unknown provider");
    }

    #[test]
    fn log_lines_count_back_from_the_end() {
        let c = "a\nb\nc\nd\n";
        assert_eq!(read_log_lines(c, 2, 0).unwrap(), "c\nd");
        assert_eq!(read_log_lines(c, 2, 1).unwrap(), "b\nc");
        assert_eq!(read_log_lines(c, 10, 0).unwrap(), "a\nb\nc\nd");
        assert_eq!(read_log_lines(c, 2, 4).unwrap(), "");
        assert_eq!(read_log_lines(c, 2, -3).unwrap(), "c\nd");
        assert_eq!(read_log_lines("", 5, 0).unwrap(), "");
        assert_eq!(
            read_log_lines(c, 0, 0).unwrap_err(),
            "limit must be positive"
        );
    }

    #[test]
    fn clean_path_matches_go() {
        for (i, o) in [
            ("/a/b/../c/", "/a/c"),
            ("/a//b/./c", "/a/b/c"),
            ("/..", "/"),
            ("a/../..", ".."),
            ("", "."),
            ("/", "/"),
        ] {
            assert_eq!(
                clean_path(i),
                o.replace('/', std::path::MAIN_SEPARATOR_STR),
                "{i:?}"
            );
        }
    }
}
