//! Port of `internal/watcher`: scans a watch folder on an interval and queues stable files (or, in
//! single-NZB-per-folder mode, whole top-level subfolders) for upload.

pub mod globmatch;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, Local, NaiveTime, TimeZone};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::config::{Par2Config, WatcherConfig};
use crate::par2::existing::reusable_set_for;
use crate::par2::{existing::set_source_for, is_par2_file};
use crate::queue::{AddOptions, Queue};
use globmatch::filepath_match;

const CACHE_CLEANUP_INTERVAL: Duration = Duration::from_secs(3600);
const CACHE_MAX_AGE: Duration = Duration::from_secs(24 * 3600);
const DEFAULT_MIN_FILE_AGE: Duration = Duration::from_secs(60);
const FOLDER_PREFIX: &str = "FOLDER:";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WatcherScheduleInfo {
    pub start_time: String,
    pub end_time: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WatcherStatusInfo {
    pub name: String,
    pub enabled: bool,
    pub initialized: bool,
    pub watch_directory: String,
    pub check_interval: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub next_run: String,
    pub is_within_schedule: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<WatcherScheduleInfo>,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub error: String,
}

/// The queue operations the watcher needs.
pub trait WatchQueue: Send + Sync {
    fn is_path_in_queue(&self, path: &str) -> Result<bool, String>;
    fn add_file_with_options(&self, path: &str, size: i64, opts: AddOptions) -> Result<(), String>;
}

impl WatchQueue for Queue {
    fn is_path_in_queue(&self, path: &str) -> Result<bool, String> {
        Queue::is_path_in_queue(self, path).map_err(|e| e.to_string())
    }
    fn add_file_with_options(&self, path: &str, size: i64, opts: AddOptions) -> Result<(), String> {
        Queue::add_file_with_options(self, path, size, opts).map_err(|e| e.to_string())
    }
}

/// Lets the watcher skip files a running job already owns.
pub trait ProcessorInterface: Send + Sync {
    fn is_path_being_processed(&self, path: &str) -> bool;
}

impl ProcessorInterface for crate::processor::runner::Processor {
    fn is_path_being_processed(&self, path: &str) -> bool {
        crate::processor::runner::Processor::is_path_being_processed(self, path)
    }
}

type Par2ConfigFn = Arc<dyn Fn() -> Option<Par2Config> + Send + Sync>;

struct CacheEntry {
    size: u64,
    at: SystemTime,
}

pub struct Watcher {
    cfg: WatcherConfig,
    queue: Arc<dyn WatchQueue>,
    processor: Option<Arc<dyn ProcessorInterface>>,
    watch_folder: String,
    size_cache: Mutex<HashMap<String, CacheEntry>>,
    next_run: RwLock<DateTime<Local>>,
    /// Current PAR2 settings (read at scan time so a config change applies without a restart).
    par2_config: Option<Par2ConfigFn>,
    /// Set while a scan runs, so a manual trigger never overlaps a periodic scan (or vice versa).
    scanning: std::sync::atomic::AtomicBool,
}

/// `HH:MM` strings compare lexicographically, as in Go.
fn hhmm(t: DateTime<Local>) -> String {
    t.format("%H:%M").to_string()
}

fn within(start: &str, end: &str, now: &str) -> bool {
    if start <= end {
        now >= start && now <= end
    } else {
        now >= start || now <= end
    }
}

fn scheduled(cfg: &WatcherConfig) -> bool {
    !cfg.schedule.start_time.is_empty() && !cfg.schedule.end_time.is_empty()
}

impl Watcher {
    pub fn new(
        cfg: WatcherConfig,
        queue: Arc<dyn WatchQueue>,
        processor: Option<Arc<dyn ProcessorInterface>>,
        watch_folder: &str,
    ) -> Self {
        Self {
            cfg,
            queue,
            processor,
            watch_folder: watch_folder.into(),
            size_cache: Mutex::new(HashMap::new()),
            next_run: RwLock::new(Local::now()),
            par2_config: None,
            scanning: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Lets the watcher see the PAR2 settings: in one-NZB-per-file mode a source's job posts its existing
    /// PAR2 set inside its own NZB, so those files must not be queued as separate jobs (issue #184).
    pub fn with_par2_config(mut self, get: Par2ConfigFn) -> Self {
        self.par2_config = Some(get);
        self
    }

    /// Runs one scan unless another is in progress (then `None`).
    fn scan_exclusive(&self, c: &CancellationToken) -> Option<Result<(), String>> {
        use std::sync::atomic::Ordering;
        if self.scanning.swap(true, Ordering::SeqCst) {
            eprintln!("watcher: a scan is already running, skipping");
            return None;
        }
        struct Reset<'a>(&'a std::sync::atomic::AtomicBool);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::SeqCst);
            }
        }
        let _reset = Reset(&self.scanning);
        Some(self.scan_directory(c).map(|_| ()))
    }

    fn interval(&self) -> Duration {
        u64::try_from(self.cfg.check_interval.nanos())
            .ok()
            .filter(|n| *n > 0)
            .map_or(Duration::from_secs(60), Duration::from_nanos)
    }

    /// Scans on every interval until cancelled (skipping scans outside the schedule).
    pub async fn start(self: Arc<Self>, cancel: CancellationToken) {
        eprintln!(
            "watcher: starting directory watching {} with interval {:?}",
            self.watch_folder,
            self.interval()
        );
        let mut scan = tokio::time::interval(self.interval());
        // a scan longer than the interval must not trigger back-to-back catch-up scans
        scan.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        scan.tick().await;
        let mut cleanup = tokio::time::interval(CACHE_CLEANUP_INTERVAL);
        cleanup.tick().await;
        self.update_next_run_time();
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                _ = scan.tick() => {
                    self.update_next_run_time();
                    if self.is_within_schedule() {
                        let me = self.clone();
                        let c = cancel.clone();
                        if let Ok(Some(Err(e))) = tokio::task::spawn_blocking(move || me.scan_exclusive(&c)).await {
                            eprintln!("watcher: error scanning directory: {e}");
                        }
                    } else {
                        eprintln!("watcher: not within schedule, skipping scan");
                    }
                }
                _ = cleanup.tick() => self.cleanup_old_cache_entries(),
            }
        }
    }

    pub fn is_within_schedule(&self) -> bool {
        !scheduled(&self.cfg)
            || within(
                &self.cfg.schedule.start_time,
                &self.cfg.schedule.end_time,
                &hhmm(Local::now()),
            )
    }

    fn would_be_within_schedule(&self, t: DateTime<Local>) -> bool {
        !scheduled(&self.cfg)
            || within(
                &self.cfg.schedule.start_time,
                &self.cfg.schedule.end_time,
                &hhmm(t),
            )
    }

    /// The next time a scan could run inside the schedule.
    fn next_scheduled_run(&self, now: DateTime<Local>) -> DateTime<Local> {
        let (start, end) = (&self.cfg.schedule.start_time, &self.cfg.schedule.end_time);
        let (h, m) = if start.len() >= 5 {
            let mut it = start.split(':');
            (
                it.next()
                    .and_then(|v| v.trim().parse::<u32>().ok())
                    .unwrap_or(0),
                it.next()
                    .and_then(|v| v.trim().parse::<u32>().ok())
                    .unwrap_or(0),
            )
        } else {
            (0, 0)
        };
        let today = now
            .date_naive()
            .and_time(NaiveTime::from_hms_opt(h.min(23), m.min(59), 0).unwrap_or_default());
        let today_start = Local.from_local_datetime(&today).earliest().unwrap_or(now);
        let cur = hhmm(now);
        if start <= end && cur > *end {
            today_start + chrono::Duration::hours(24)
        } else if start > end && cur > *end && cur < *start {
            today_start
        } else if today_start < now && start <= end {
            today_start + chrono::Duration::hours(24)
        } else {
            today_start
        }
    }

    fn update_next_run_time(&self) {
        let now = Local::now();
        let mut next = now
            + chrono::Duration::from_std(self.interval())
                .unwrap_or_else(|_| chrono::Duration::minutes(1));
        if scheduled(&self.cfg) && !self.would_be_within_schedule(next) {
            next = self.next_scheduled_run(now);
        }
        *self.next_run.write().unwrap_or_else(|e| e.into_inner()) = next;
    }

    pub fn next_run_time(&self) -> DateTime<Local> {
        *self.next_run.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Runs one scan now in the background (only inside the schedule).
    pub fn trigger_scan(self: &Arc<Self>, cancel: &CancellationToken) {
        let (me, c) = (self.clone(), cancel.clone());
        tokio::task::spawn_blocking(move || {
            if me.is_within_schedule() {
                eprintln!(
                    "watcher: watch scanning started: watch_directory={}",
                    me.watch_folder
                );
                if let Some(Err(e)) = me.scan_exclusive(&c) {
                    eprintln!("watcher: error in triggered directory scan: {e}");
                }
            }
        });
    }

    pub fn watcher_status(&self) -> WatcherStatusInfo {
        let mut s = WatcherStatusInfo {
            name: self.cfg.name.clone(),
            enabled: self.cfg.enabled,
            initialized: true,
            watch_directory: self.watch_folder.clone(),
            check_interval: self.cfg.check_interval.0.clone(),
            next_run: String::new(),
            is_within_schedule: self.is_within_schedule(),
            schedule: None,
            error: String::new(),
        };
        if self.cfg.enabled {
            s.next_run = self
                .next_run_time()
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
            if scheduled(&self.cfg) {
                s.schedule = Some(WatcherScheduleInfo {
                    start_time: self.cfg.schedule.start_time.clone(),
                    end_time: self.cfg.schedule.end_time.clone(),
                });
            }
        }
        s
    }

    /// Visits every regular file under the watch folder in lexical order. An unreadable entry is logged
    /// and skipped (a permission-denied subfolder must not stop every other import); only an unreadable
    /// root is an error. Symlinks are skipped unless `follow_symlinks`.
    fn walk_files(
        &self,
        cancel: &CancellationToken,
        f: &mut dyn FnMut(&str, &std::fs::Metadata),
    ) -> Result<(), String> {
        std::fs::read_dir(&self.watch_folder).map_err(|e| e.to_string())?;
        self.walk(Path::new(&self.watch_folder), cancel, f);
        Ok(())
    }

    fn walk(
        &self,
        dir: &Path,
        cancel: &CancellationToken,
        f: &mut dyn FnMut(&str, &std::fs::Metadata),
    ) {
        if cancel.is_cancelled() {
            return;
        }
        let mut entries: Vec<_> = match std::fs::read_dir(dir) {
            Ok(r) => r.flatten().collect(),
            Err(e) => {
                return eprintln!(
                    "watcher: skipping unreadable entry in watch folder: path={} error={e}",
                    dir.display()
                );
            }
        };
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for e in entries {
            let path = e.path();
            let Ok(lmeta) = std::fs::symlink_metadata(&path) else {
                continue;
            };
            if lmeta.is_dir() {
                self.walk(&path, cancel, f);
                continue;
            }
            if !self.cfg.follow_symlinks && lmeta.file_type().is_symlink() {
                continue;
            }
            // `entry.Info()` is the entry's own (lstat) info, like Go's WalkDir
            f(&path.to_string_lossy(), &lmeta);
        }
    }

    fn passes_filters(&self, path: &str, size: u64) -> bool {
        if i64::try_from(size).unwrap_or(i64::MAX) < self.cfg.min_file_size {
            return false;
        }
        let base = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        for pattern in self.cfg.ignore_patterns.iter().flatten() {
            match filepath_match(pattern, &base) {
                Ok(true) => return false,
                Ok(false) => {}
                Err(_) => eprintln!("watcher: invalid pattern: {pattern}"),
            }
        }
        true
    }

    fn should_process_file(&self, path: &str, meta: &std::fs::Metadata) -> bool {
        self.passes_filters(path, meta.len()) && self.is_file_stable(path, meta)
    }

    /// Stable = untouched for `min_file_age` (default 60 s) and the same size as at the previous scan.
    fn is_file_stable(&self, path: &str, meta: &std::fs::Metadata) -> bool {
        let min_age = u64::try_from(self.cfg.min_file_age.nanos())
            .ok()
            .filter(|n| *n > 0)
            .map_or(DEFAULT_MIN_FILE_AGE, Duration::from_nanos);
        let age = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .unwrap_or_default();
        if age < min_age {
            return false;
        }
        self.is_file_size_stable(path, meta.len())
    }

    fn is_file_size_stable(&self, path: &str, size: u64) -> bool {
        let mut c = self.size_cache.lock().unwrap_or_else(|e| e.into_inner());
        let prev = c.insert(
            path.to_string(),
            CacheEntry {
                size,
                at: SystemTime::now(),
            },
        );
        prev.is_some_and(|p| p.size == size)
    }

    /// Drops cache entries older than 24 hours.
    pub fn cleanup_old_cache_entries(&self) {
        let cutoff = SystemTime::now()
            .checked_sub(CACHE_MAX_AGE)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        self.size_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|_, e| e.at >= cutoff);
    }

    fn par2_enabled(&self) -> bool {
        self.par2_config
            .as_ref()
            .and_then(|f| f())
            .is_some_and(|c| c.enabled == Some(true))
    }

    /// True when `path` is a PAR2 file its sibling source's job will post inside the source's own NZB.
    /// Only when the sibling will itself be queued (exists, regular, passes the filters) and the set
    /// passes the same reuse check the posting job applies. Known limitation (as in Go): a sibling posted
    /// before its PAR2 files appeared leaves them unposted.
    fn posted_with_sibling(&self, path: &str, cache: &mut HashMap<String, Vec<String>>) -> bool {
        if !self.par2_enabled() || !is_par2_file(path) {
            return false;
        }
        let Some(source) = set_source_for(path) else {
            return false;
        };
        let set = cache
            .entry(source.clone())
            .or_insert_with(|| self.reusable_sibling_set(&source));
        set.iter().any(|p| p == path)
    }

    fn reusable_sibling_set(&self, source: &str) -> Vec<String> {
        match std::fs::metadata(source) {
            Ok(m) if m.is_file() && self.passes_filters(source, m.len()) => {
                reusable_set_for(source).unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }

    fn being_processed(&self, path: &str) -> bool {
        self.processor
            .as_ref()
            .is_some_and(|p| p.is_path_being_processed(path))
    }

    fn calculate_directory_size(&self, cancel: &CancellationToken) -> Result<u64, String> {
        let mut total = 0u64;
        self.walk_files(cancel, &mut |p, m| {
            if self.passes_filters(p, m.len()) {
                total += m.len();
            }
        })?;
        Ok(total)
    }

    /// One scan: skips when the folder is below `size_threshold`, then queues new stable files (or folders).
    pub fn scan_directory(&self, cancel: &CancellationToken) -> Result<(), String> {
        let total = self
            .calculate_directory_size(cancel)
            .map_err(|e| format!("failed to calculate directory size: {e}"))?;
        eprintln!(
            "watcher: directory size check: directory={} total_size={total} threshold={}",
            self.watch_folder, self.cfg.size_threshold
        );
        if i64::try_from(total).unwrap_or(i64::MAX) < self.cfg.size_threshold {
            return Ok(());
        }
        if self.cfg.single_nzb_per_folder {
            return self.scan_group_by_folder(cancel);
        }
        let mut sets = HashMap::new();
        let mut files: Vec<(String, u64)> = Vec::new();
        self.walk_files(cancel, &mut |p, m| {
            if self.should_process_file(p, m) {
                files.push((p.to_string(), m.len()));
            }
        })?;
        for (path, size) in files {
            self.queue_individual(&path, size, &mut sets);
        }
        Ok(())
    }

    /// Queues one file unless it is a sibling PAR2, running, or already tracked.
    fn queue_individual(&self, path: &str, size: u64, sets: &mut HashMap<String, Vec<String>>) {
        if self.posted_with_sibling(path, sets) || self.being_processed(path) {
            return;
        }
        match self.queue.is_path_in_queue(path) {
            Ok(true) => return,
            Ok(false) => {}
            Err(e) => {
                return eprintln!(
                    "watcher: error checking if path is in queue: path={path} error={e}"
                );
            }
        }
        // the watcher's own root lets the processor derive the relative output path (issue #168)
        let opts = AddOptions {
            input_folder: self.watch_folder.clone(),
            ..Default::default()
        };
        match self
            .queue
            .add_file_with_options(path, i64::try_from(size).unwrap_or(i64::MAX), opts)
        {
            Ok(()) => eprintln!(
                "watcher: added file to queue: path={} size={size}",
                Path::new(path)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            ),
            Err(e) => eprintln!("watcher: error adding file to queue: path={path} error={e}"),
        }
    }

    /// Single-NZB-per-folder mode: top-level subfolders become `FOLDER:` jobs, loose files individual ones.
    fn scan_group_by_folder(&self, cancel: &CancellationToken) -> Result<(), String> {
        let mut by_folder: std::collections::BTreeMap<String, (Vec<String>, u64)> =
            std::collections::BTreeMap::new();
        let mut individual: Vec<(String, u64)> = Vec::new();
        let root = PathBuf::from(&self.watch_folder);
        let mut fail: Option<String> = None;
        self.walk_files(cancel, &mut |p, m| {
            if !self.should_process_file(p, m) {
                return;
            }
            let Ok(rel) = Path::new(p).strip_prefix(&root) else {
                fail = Some(format!(
                    "Rel: can't make {p} relative to {}",
                    self.watch_folder
                ));
                return;
            };
            let mut comps = rel.components();
            let first = comps
                .next()
                .map(|c| c.as_os_str().to_string_lossy().into_owned())
                .unwrap_or_default();
            if comps.next().is_none() {
                individual.push((p.to_string(), m.len()));
            } else {
                let e = by_folder
                    .entry(root.join(first).to_string_lossy().into_owned())
                    .or_default();
                e.0.push(p.to_string());
                e.1 += m.len();
            }
        })?;
        if let Some(f) = fail {
            return Err(f);
        }
        for (folder, (files, size)) in by_folder {
            if files.is_empty() {
                continue;
            }
            let job = format!("{FOLDER_PREFIX}{folder}");
            if self.being_processed(&job) {
                eprintln!(
                    "watcher: folder is currently being processed, skipping: folder={folder}"
                );
                continue;
            }
            match self.queue.is_path_in_queue(&job) {
                Ok(true) => continue,
                Ok(false) => {}
                Err(e) => eprintln!(
                    "watcher: error checking if folder is in queue: folder={folder} error={e}"
                ),
            }
            let busy = files.iter().any(|f| {
                self.being_processed(f) || self.queue.is_path_in_queue(f).unwrap_or(false)
            });
            if busy {
                continue;
            }
            eprintln!(
                "watcher: adding folder to queue: folder={} files={} size={size}",
                Path::new(&folder)
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy(),
                files.len()
            );
            let opts = AddOptions {
                input_folder: self.watch_folder.clone(),
                ..Default::default()
            };
            if let Err(e) = self.queue.add_file_with_options(
                &job,
                i64::try_from(size).unwrap_or(i64::MAX),
                opts,
            ) {
                eprintln!("watcher: error adding folder to queue: folder={folder} error={e}");
            }
        }
        let mut sets = HashMap::new();
        for (path, size) in individual {
            self.queue_individual(&path, size, &mut sets);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
