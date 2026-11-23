//! Port of `internal/verification`: the durable, lease-based post-upload verification service.
//!
//! Completed transfers are recorded durably and verified in the background instead of holding an
//! upload slot through the propagation delay. The service streams a file's manifest, STATs the
//! articles and persists only the misses; misses are then re-posted (with their exact Message-ID and
//! headers from the manifest) or STAT-rechecked with exponential backoff, using database leases so the
//! work survives restarts. It talks to the network only through [`Stater`] and [`Reposter`].

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::manifest::{ArticleRecord, ManifestError, Reader};
use crate::poster::BoxFut;
use crate::transferstore::{
    self as ts, PendingFile, Store, StoreError, TransferFile, VerificationFailure,
};

/// Reports whether articles exist. `Ok(true)` means *missing* and is only returned when the server
/// positively confirmed it (a `430`); an error means the check itself failed, so presence is UNKNOWN
/// and must not spend repost/recheck budget. For `stat_batch` the returned set holds the ids NOT
/// confirmed present (a per-id error counts as missing; the per-failure `stat` re-validates before any
/// budget is spent).
pub trait Stater: Send + Sync {
    fn stat<'a>(&'a self, message_id: &'a str) -> BoxFut<'a, std::result::Result<bool, String>>;
    fn stat_batch<'a>(
        &'a self,
        ids: &'a [String],
    ) -> BoxFut<'a, std::result::Result<HashSet<String>, String>>;
}

/// Why a re-post failed: a missing source file can never succeed, anything else may be retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RepostError {
    SourceMissing(String),
    Other(String),
}

impl std::fmt::Display for RepostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::SourceMissing(m) | Self::Other(m) => f.write_str(m),
        }
    }
}

/// Re-posts a single article described by its manifest record.
pub trait Reposter: Send + Sync {
    fn repost<'a>(
        &'a self,
        rec: &'a ArticleRecord,
    ) -> BoxFut<'a, std::result::Result<(), RepostError>>;
}

impl Reposter for crate::poster::Reposter {
    fn repost<'a>(
        &'a self,
        rec: &'a ArticleRecord,
    ) -> BoxFut<'a, std::result::Result<(), RepostError>> {
        Box::pin(async move {
            // the service's own cancellation is checked between failures; a re-post is not interrupted
            crate::poster::Reposter::repost(self, &CancellationToken::new(), rec)
                .await
                .map_err(|e| match e {
                    crate::poster::PostError::SourceMissing(m) => RepostError::SourceMissing(m),
                    other => RepostError::Other(other.to_string()),
                })
        })
    }
}

/// Post-verification cleanup of a fully verified transfer (deletes originals per policy, removes PAR2
/// files and manifests). A no-op while the transfer is not terminal.
pub trait Cleaner: Send + Sync {
    fn cleanup_transfer<'a>(
        &'a self,
        transfer_id: &'a str,
    ) -> BoxFut<'a, std::result::Result<bool, String>>;
}

/// Timing and concurrency; zero values fall back to conservative defaults (see [`Config::with_defaults`]).
#[derive(Debug, Clone)]
pub struct Config {
    /// Message-IDs per `stat_batch` call. 0 = 100.
    pub stat_batch_size: usize,
    /// How long to wait after a (re)post before checking. 0 = 10 s.
    pub propagation_delay: Duration,
    /// Re-posts before falling back to STAT-only deferred checking.
    pub max_reposts: i64,
    /// Base delay between STAT-only rechecks; grows exponentially up to `max_backoff`.
    pub deferred_backoff: Duration,
    pub max_backoff: Duration,
    /// STAT-only rechecks before the article (and its file) is permanently failed.
    pub max_deferred_checks: i64,
    /// How long a claimed batch is owned before another worker may reclaim it.
    pub lease_duration: Duration,
    /// Maximum failures claimed per cycle.
    pub batch_size: i64,
    /// How often the run loop looks for due work.
    pub poll_interval: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            stat_batch_size: 0,
            propagation_delay: Duration::zero(),
            max_reposts: 0,
            deferred_backoff: Duration::zero(),
            max_backoff: Duration::zero(),
            max_deferred_checks: 0,
            lease_duration: Duration::zero(),
            batch_size: 0,
            poll_interval: Duration::zero(),
        }
    }
}

impl Config {
    pub fn with_defaults(mut self) -> Self {
        let or = |d: Duration, v: Duration| if d <= Duration::zero() { v } else { d };
        if self.stat_batch_size == 0 {
            self.stat_batch_size = 100;
        }
        self.propagation_delay = or(self.propagation_delay, Duration::seconds(10));
        self.max_reposts = self.max_reposts.max(0);
        self.deferred_backoff = or(self.deferred_backoff, Duration::seconds(30));
        self.max_backoff = or(self.max_backoff, Duration::minutes(5));
        if self.max_deferred_checks <= 0 {
            self.max_deferred_checks = 5;
        }
        self.lease_duration = or(self.lease_duration, Duration::minutes(5));
        if self.batch_size <= 0 {
            self.batch_size = 10_000;
        }
        self.poll_interval = or(self.poll_interval, Duration::seconds(15));
        self
    }
}

#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("{0}")]
    Store(#[from] StoreError),
    #[error("context canceled")]
    Canceled,
    #[error("{0}")]
    Other(String),
}

type Result<T> = std::result::Result<T, VerifyError>;

const STATUS_VERIFIED: &str = "verified";
const STATUS_FAILED: &str = "verification_failed";
/// Poll ticks between periodic `reconcile_stuck` passes (~5 minutes at the default interval).
const RECONCILE_EVERY_TICKS: u64 = 20;
/// Consecutive cycles the busy gate may defer before a full cycle is forced (~5 minutes): without
/// it a continuously saturated upload pool would starve verification and repost recovery forever.
const MAX_BUSY_SKIPS: usize = 20;

fn log(msg: &str) {
    crate::logx::warn(&format!("verification: {msg}"), &[]);
}

/// Where one completed item stands in verification, for the dashboard's verification panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Activity {
    pub id: String,
    pub path: String,
    /// `checking` (STAT sweep running), `repairing` (missing articles being re-posted or
    /// rechecked), `waiting` (propagation delay before the next check), `deferred` (held back while
    /// uploads use every connection) or `queued` (due, picked up on the next cycle).
    pub phase: &'static str,
    pub total_articles: i64,
    pub verified_articles: i64,
    pub missing_articles: i64,
    /// RFC 3339; set while `waiting`.
    pub next_check_at: Option<String>,
}

/// Live STAT-sweep progress of one file: (articles checked, misses so far).
type LiveKey = (String, String);

/// Drops a file's live progress entry however its sweep ends.
struct LiveGuard<'a> {
    live: &'a Mutex<HashMap<LiveKey, (i64, i64)>>,
    key: LiveKey,
}

impl LiveGuard<'_> {
    fn set(&self, checked: i64, missing: i64) {
        lock(self.live).insert(self.key.clone(), (checked, missing));
    }
}

impl Drop for LiveGuard<'_> {
    fn drop(&mut self) {
        lock(self.live).remove(&self.key);
    }
}

fn file_name(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map_or_else(|| path.to_string(), |n| n.to_string_lossy().into_owned())
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Folds one item's files (and the live sweeps among them) into an [`Activity`].
fn item_activity(
    files: &[PendingFile],
    live: &HashMap<LiveKey, (i64, i64)>,
    deferred: bool,
    now: &str,
) -> Activity {
    let (mut total, mut verified, mut missing) = (0, 0, 0);
    let (mut checking, mut next): (bool, Option<&str>) = (false, None);
    for f in files {
        total += f.article_count;
        let sweep = live.get(&(f.transfer_id.clone(), f.file_id.clone()));
        match (f.verification_state.as_str(), sweep) {
            (_, Some(&(checked, misses))) => {
                checking = true;
                verified += (checked - misses).max(0);
                missing += misses;
            }
            (ts::STATE_VERIFIED, None) => verified += f.article_count,
            (ts::STATE_VERIFYING | ts::STATE_VERIFICATION_FAILED, None) => {
                verified += (f.article_count - f.open_failures).max(0);
                missing += f.open_failures;
            }
            _ => {
                if let Some(at) = f.next_check_at.as_deref().filter(|at| *at > now) {
                    next = Some(next.map_or(at, |n| n.min(at)));
                }
            }
        }
    }
    let phase = if checking {
        "checking"
    } else if missing > 0 {
        "repairing"
    } else if next.is_some() {
        "waiting"
    } else if deferred {
        "deferred"
    } else {
        "queued"
    };
    Activity {
        id: files[0].completed_item_id.clone(),
        path: files[0].path.clone(),
        phase,
        total_articles: total,
        verified_articles: verified.min(total),
        missing_articles: missing,
        next_check_at: (phase == "waiting").then(|| next.unwrap_or_default().to_string()),
    }
}

type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

pub struct Service {
    store: Store,
    stater: Arc<dyn Stater>,
    reposter: Arc<dyn Reposter>,
    cfg: Config,
    owner: String,
    now: Clock,
    cleaner: Option<Arc<dyn Cleaner>>,
    busy: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    busy_skips: AtomicUsize,
    /// Set while the busy gate holds cycles back.
    deferred: AtomicBool,
    live: Mutex<HashMap<LiveKey, (i64, i64)>>,
}

#[derive(PartialEq, Eq, Hash, Clone)]
struct RecordKey(String, String, i64);

impl Service {
    /// `owner` identifies this worker for lease ownership (for example hostname + pid).
    pub fn new(
        store: Store,
        stater: Arc<dyn Stater>,
        reposter: Arc<dyn Reposter>,
        cfg: Config,
        owner: &str,
    ) -> Self {
        Self {
            store,
            stater,
            reposter,
            cfg: cfg.with_defaults(),
            owner: owner.to_string(),
            now: Arc::new(Utc::now),
            cleaner: None,
            busy: None,
            busy_skips: AtomicUsize::new(0),
            deferred: AtomicBool::new(false),
            live: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_clock(mut self, now: Clock) -> Self {
        self.now = now;
        self
    }

    /// Installs the cleaner run when a transfer becomes fully verified.
    pub fn set_cleaner(&mut self, c: Arc<dyn Cleaner>) {
        self.cleaner = Some(c);
    }

    /// Installs a predicate consulted before each cycle; while it is true the sweep is deferred, so
    /// background STAT sweeps do not steal connections from saturated live uploads (issue #168).
    pub fn set_busy_check(&mut self, f: Arc<dyn Fn() -> bool + Send + Sync>) {
        self.busy = Some(f);
    }

    fn now(&self) -> DateTime<Utc> {
        (self.now)()
    }

    /// Every completed item awaiting verification and where it stands, oldest first.
    pub fn activity(&self) -> Result<Vec<Activity>> {
        let files = self.store.pending_verification_files()?;
        let live = lock(&self.live).clone();
        let deferred = self.deferred.load(Ordering::Relaxed);
        let now = ts::fmt_time(self.now());
        Ok(files
            .chunk_by(|a, b| a.completed_item_id == b.completed_item_id)
            .map(|item| item_activity(item, &live, deferred, &now))
            .collect())
    }

    /// `base << min(n, 16)` capped at `max_backoff`.
    fn backoff(&self, n: i64) -> Duration {
        let shift = n.clamp(0, 16) as u32;
        self.cfg
            .deferred_backoff
            .num_milliseconds()
            .checked_mul(1 << shift)
            .map(Duration::milliseconds)
            .filter(|b| *b > Duration::zero() && *b <= self.cfg.max_backoff)
            .unwrap_or(self.cfg.max_backoff)
    }

    /// Reflects a transfer's terminal verification outcome onto its completed item and, on full
    /// success, runs cleanup. A no-op until every file is terminal; failure retains all recovery data.
    async fn finalize_transfer(&self, transfer_id: &str) {
        let files = match self.store.list_files_by_transfer(transfer_id) {
            Ok(f) => f,
            Err(e) => {
                return log(&format!(
                    "list files for finalize failed: transfer={transfer_id} error={e}"
                ));
            }
        };
        if files.is_empty() {
            return;
        }
        let mut any_failed = false;
        let mut completed_item_id = String::new();
        for f in &files {
            if !f.completed_item_id.is_empty() {
                completed_item_id.clone_from(&f.completed_item_id);
            }
            match f.verification_state.as_str() {
                ts::STATE_VERIFIED => {}
                ts::STATE_VERIFICATION_FAILED => any_failed = true,
                _ => return, // not terminal yet
            }
        }
        // Files can go terminal before the processor linked the completed item; finalizing now would
        // skip the status update AND run destructive cleanup for an item still pending_verification.
        // The periodic reconcile pass finalizes it once the link exists.
        if completed_item_id.is_empty() {
            return log(&format!(
                "transfer terminal but completed item not linked yet; deferring finalize: transfer={transfer_id}"
            ));
        }
        let status = if any_failed {
            STATUS_FAILED
        } else {
            STATUS_VERIFIED
        };
        if let Err(e) = self
            .store
            .set_completed_item_verification_status(&completed_item_id, status)
        {
            log(&format!(
                "update completed item status failed: transfer={transfer_id} error={e}"
            ));
        }
        let name = files
            .iter()
            .find(|f| f.file_role == crate::manifest::ROLE_ORIGINAL)
            .map_or_else(
                || file_name(&files[0].source_path),
                |f| file_name(&f.source_path),
            );
        let attrs = [("file", name.into()), ("files", files.len().into())];
        if any_failed {
            crate::logx::error(
                "Verification failed: some articles could not be recovered",
                &attrs,
            );
            return;
        }
        crate::logx::info("Upload verified", &attrs);
        if let Some(c) = &self.cleaner
            && let Err(e) = c.cleanup_transfer(transfer_id).await
        {
            log(&format!("cleanup failed: transfer={transfer_id} error={e}"));
        }
    }

    /// Drives verification until `cancel` fires: reclaim expired leases, first checks for due files,
    /// then due failures; stuck items are reconciled at startup and periodically.
    pub async fn run(&self, cancel: &CancellationToken) {
        if let Err(e) = self.reconcile_stuck(cancel).await {
            log(&format!("startup reconciliation failed: {e}"));
        }
        let mut ticks = 0u64;
        loop {
            self.run_once(cancel).await;
            ticks += 1;
            if ticks.is_multiple_of(RECONCILE_EVERY_TICKS)
                && let Err(e) = self.reconcile_stuck(cancel).await
            {
                log(&format!("periodic reconciliation failed: {e}"));
            }
            let wait = self
                .cfg
                .poll_interval
                .to_std()
                .unwrap_or(std::time::Duration::from_secs(15));
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(wait) => {}
            }
        }
    }

    /// One verification cycle.
    pub async fn run_once(&self, cancel: &CancellationToken) {
        let now = self.now();
        if let Err(e) = self.store.reclaim_expired_leases(now) {
            log(&format!("reclaim leases failed: {e}"));
        }
        if let Some(busy) = &self.busy
            && busy()
        {
            if self.busy_skips.load(Ordering::Relaxed) < MAX_BUSY_SKIPS {
                self.busy_skips.fetch_add(1, Ordering::Relaxed);
                self.deferred.store(true, Ordering::Relaxed);
                return;
            }
            log("upload pool busy but verification deferred too long; running cycle anyway");
        }
        self.busy_skips.store(0, Ordering::Relaxed);
        self.deferred.store(false, Ordering::Relaxed);
        if let Err(e) = self
            .verify_due_files(cancel, now, self.cfg.batch_size)
            .await
        {
            log(&format!("verify due files failed: {e}"));
        }
        if let Err(e) = self.process_due_failures(cancel, now).await {
            log(&format!("process due failures failed: {e}"));
        }
    }

    /// First check for every file whose check is due, up to `limit`. A failure on one file is logged
    /// and does not abort the batch. Returns the number verified.
    pub async fn verify_due_files(
        &self,
        cancel: &CancellationToken,
        now: DateTime<Utc>,
        limit: i64,
    ) -> Result<usize> {
        let due = self.store.list_due_files(now, limit)?;
        let mut processed = 0;
        for tf in due {
            if cancel.is_cancelled() {
                return Err(VerifyError::Canceled);
            }
            match self.verify_file(cancel, &tf).await {
                Ok(()) => processed += 1,
                Err(e) => log(&format!(
                    "verify file failed: transfer={} file={} error={e}",
                    tf.transfer_id, tf.file_id
                )),
            }
        }
        Ok(processed)
    }

    /// STATs every article of a file's manifest in batches and persists only the misses. No misses:
    /// verified. Otherwise `verifying`, with the misses left for `process_due_failures`.
    pub async fn verify_file(&self, cancel: &CancellationToken, tf: &TransferFile) -> Result<()> {
        let mut reader = match Reader::open(std::path::Path::new(&tf.manifest_path)) {
            Ok(r) => r,
            Err(ManifestError::Open(e)) if e.kind() == io::ErrorKind::NotFound => {
                // The manifest will never come back: fail the file instead of retrying forever, or
                // the completed item stays pending_verification across restarts (issue #168).
                return self
                    .fail_file_terminally(tf, &format!("manifest missing: {e}"))
                    .await;
            }
            Err(e) => return self.defer_file_check(tf, &e.to_string()).await,
        };
        let live = LiveGuard {
            live: &self.live,
            key: (tf.transfer_id.clone(), tf.file_id.clone()),
        };
        live.set(0, 0);
        crate::logx::info(
            "Verifying upload: checking every article is on the server",
            &[
                ("file", file_name(&tf.source_path).into()),
                ("articles", tf.article_count.into()),
            ],
        );
        let mut checked = 0i64;
        let mut missing: Vec<ArticleRecord> = Vec::new();
        let mut chunk: Vec<ArticleRecord> = Vec::with_capacity(self.cfg.stat_batch_size);
        loop {
            let rec = match reader.next_record() {
                Ok(Some(r)) => r,
                Ok(None) => break,
                // unreadable or corrupt content: back off (and eventually terminalize)
                Err(e) => return self.defer_file_check(tf, &e.to_string()).await,
            };
            if cancel.is_cancelled() {
                return Err(VerifyError::Canceled);
            }
            chunk.push(rec);
            if chunk.len() >= self.cfg.stat_batch_size {
                let n = chunk.len() as i64;
                if let Err(e) = self.flush(&mut chunk, &mut missing).await {
                    return if cancel.is_cancelled() {
                        Err(VerifyError::Canceled)
                    } else {
                        self.defer_file_check(tf, &e).await
                    };
                }
                checked += n;
                live.set(checked, missing.len() as i64);
            }
        }
        if let Err(e) = self.flush(&mut chunk, &mut missing).await {
            return if cancel.is_cancelled() {
                Err(VerifyError::Canceled)
            } else {
                self.defer_file_check(tf, &e).await
            };
        }
        if cancel.is_cancelled() {
            return Err(VerifyError::Canceled);
        }
        drop(live);
        if missing.is_empty() {
            self.store.set_verification_state(
                &tf.transfer_id,
                &tf.file_id,
                ts::STATE_VERIFIED,
                None,
                "",
            )?;
            crate::logx::info(
                "File verified: every article is on the server",
                &[
                    ("file", file_name(&tf.source_path).into()),
                    ("articles", tf.article_count.into()),
                ],
            );
            self.finalize_transfer(&tf.transfer_id).await;
            return Ok(());
        }
        // persist the misses for durable re-posting/rechecking
        let next = self.now() + self.cfg.propagation_delay;
        let failures: Vec<VerificationFailure> = missing
            .iter()
            .map(|rec| VerificationFailure {
                transfer_id: tf.transfer_id.clone(),
                file_id: tf.file_id.clone(),
                article_index: rec.index,
                message_id: rec.message_id.clone(),
                groups: rec.groups.clone(),
                state: ts::FAILURE_PENDING.into(),
                next_attempt_at: next,
                ..Default::default()
            })
            .collect();
        // one transaction instead of a commit per missing article, off the async worker thread
        crate::blocking::in_place(|| self.store.add_failures(&failures))?;
        crate::logx::warn(
            "Articles missing after upload; they will be rechecked and re-posted",
            &[
                ("file", file_name(&tf.source_path).into()),
                ("missing", missing.len().into()),
                ("articles", tf.article_count.into()),
            ],
        );
        self.store.set_verification_state(
            &tf.transfer_id,
            &tf.file_id,
            ts::STATE_VERIFYING,
            Some(next),
            "",
        )?;
        Ok(())
    }

    /// STATs a chunk in one batched sweep and moves its misses into `missing`.
    async fn flush(
        &self,
        chunk: &mut Vec<ArticleRecord>,
        missing: &mut Vec<ArticleRecord>,
    ) -> std::result::Result<(), String> {
        if chunk.is_empty() {
            return Ok(());
        }
        let ids: Vec<String> = chunk.iter().map(|r| r.message_id.clone()).collect();
        let gone = self.stater.stat_batch(&ids).await?;
        missing.extend(chunk.drain(..).filter(|r| gone.contains(&r.message_id)));
        Ok(())
    }

    async fn fail_file_terminally(&self, tf: &TransferFile, reason: &str) -> Result<()> {
        log(&format!(
            "file failed terminally: transfer={} file={} reason={reason}",
            tf.transfer_id, tf.file_id
        ));
        self.store.set_verification_state(
            &tf.transfer_id,
            &tf.file_id,
            ts::STATE_VERIFICATION_FAILED,
            None,
            reason,
        )?;
        self.finalize_transfer(&tf.transfer_id).await;
        Ok(())
    }

    /// Reschedules a file's first check after a transient error with exponential backoff; after
    /// `max_deferred_checks` attempts the file fails terminally.
    async fn defer_file_check(&self, tf: &TransferFile, cause: &str) -> Result<()> {
        let attempts = tf.check_attempts + 1;
        if attempts >= self.cfg.max_deferred_checks {
            return self
                .fail_file_terminally(
                    tf,
                    &format!("verification check failed after repeated attempts: {cause}"),
                )
                .await;
        }
        let next = self.now() + self.backoff(attempts - 1);
        log(&format!(
            "check deferred: transfer={} file={} attempt={attempts} next_check_at={next} error={cause}",
            tf.transfer_id, tf.file_id
        ));
        self.store
            .defer_file_check(&tf.transfer_id, &tf.file_id, next, cause)?;
        Ok(())
    }

    /// Claims a batch of due failures and handles each: re-post (under `max_reposts`) or STAT-only
    /// recheck with backoff. Resolving a file's last failure verifies it; exhausting retries fails it.
    pub async fn process_due_failures(
        &self,
        cancel: &CancellationToken,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        let claimed = self.store.claim_due_failures(
            &self.owner,
            self.cfg.lease_duration,
            self.cfg.batch_size,
            now,
        )?;
        if claimed.is_empty() {
            return Ok(0);
        }
        // Pre-resolve with batched STAT sweeps: most claimed failures are propagation lag and are
        // present by now. Anything not confirmed present falls through to `handle_failure`, whose
        // per-article STAT still tells "missing" from "check errored" before any budget is spent.
        let still_missing = self.batch_pre_resolve(&claimed).await;
        let mut touched: HashSet<(String, String)> = HashSet::new();
        let mut unresolved = Vec::new();
        for mut f in claimed.iter().cloned() {
            touched.insert((f.transfer_id.clone(), f.file_id.clone()));
            if !still_missing.contains(&f.message_id) {
                f.state = ts::FAILURE_RESOLVED.into();
                f.last_error.clear();
                f.next_attempt_at = now;
                let _ = self.store.update_failure_after_check(&f);
                continue;
            }
            unresolved.push(f);
        }
        crate::logx::info(
            "Rechecked missing articles",
            &[
                ("found", (claimed.len() - unresolved.len()).into()),
                ("stillMissing", unresolved.len().into()),
            ],
        );
        let records = self.resolve_records(&unresolved);
        for f in unresolved {
            if cancel.is_cancelled() {
                return Err(VerifyError::Canceled);
            }
            self.handle_failure(f, &records, now).await;
        }
        for (t, f) in touched {
            self.reconcile_file_state(&t, &f).await;
        }
        Ok(claimed.len())
    }

    /// Ids NOT confirmed present. On a sweep error the remaining ids count as unresolved so the
    /// per-failure path decides; never "resolved", which could silently drop a real miss.
    async fn batch_pre_resolve(&self, claimed: &[VerificationFailure]) -> HashSet<String> {
        let mut seen = HashSet::new();
        let ids: Vec<String> = claimed
            .iter()
            .filter(|f| seen.insert(f.message_id.clone()))
            .map(|f| f.message_id.clone())
            .collect();
        let mut unresolved = HashSet::new();
        for (i, chunk) in ids.chunks(self.cfg.stat_batch_size).enumerate() {
            match self.stater.stat_batch(chunk).await {
                Ok(missing) => unresolved.extend(missing),
                Err(_) => {
                    unresolved.extend(ids[i * self.cfg.stat_batch_size..].iter().cloned());
                    return unresolved;
                }
            }
        }
        unresolved
    }

    /// Processes one claimed failure. It always STATs first: a miss recorded by the first sweep is
    /// usually propagation lag, so checking first turns a would-be re-upload into one cheap STAT.
    async fn handle_failure(
        &self,
        mut f: VerificationFailure,
        records: &HashMap<RecordKey, ArticleRecord>,
        now: DateTime<Utc>,
    ) {
        let missing = match self.stater.stat(&f.message_id).await {
            Err(e) => {
                // The check itself failed (timeout, dropped connection): the article may well exist.
                // Reschedule WITHOUT consuming budget, or a flaky provider re-uploads data that is
                // already there and eventually fails the file for no reason.
                f.state = ts::FAILURE_PENDING.into();
                f.next_attempt_at = now + self.cfg.deferred_backoff;
                f.last_error = format!("stat check failed: {e}");
                let _ = self.store.update_failure_after_check(&f);
                return;
            }
            Ok(m) => m,
        };
        if !missing {
            f.state = ts::FAILURE_RESOLVED.into();
            f.last_error.clear();
            f.next_attempt_at = now;
            let _ = self.store.update_failure_after_check(&f);
            return;
        }
        // Confirmed missing: re-post while budget remains and a source exists.
        if f.repost_count < self.cfg.max_reposts
            && let Some(rec) = records.get(&RecordKey(
                f.transfer_id.clone(),
                f.file_id.clone(),
                f.article_index,
            ))
        {
            match self.reposter.repost(rec).await {
                Ok(()) => {
                    f.repost_count += 1;
                    crate::logx::info(
                        "Re-posted a missing article",
                        &[
                            ("messageID", f.message_id.clone().into()),
                            ("attempt", f.repost_count.into()),
                        ],
                    );
                    f.state = ts::FAILURE_PENDING.into();
                    // propagation starts when this re-post finishes, not at cycle start
                    f.next_attempt_at = self.now() + self.cfg.propagation_delay;
                    f.last_error.clear();
                    let _ = self.store.update_failure_after_check(&f);
                    return;
                }
                Err(RepostError::SourceMissing(e)) => {
                    // The source is gone (e.g. a temp PAR2 already cleaned up): re-posting can never
                    // succeed, so fall through to the bounded deferred-recheck path.
                    log(&format!(
                        "repost source missing, falling back to STAT rechecks: messageID={} error={e}",
                        f.message_id
                    ));
                }
                Err(RepostError::Other(e)) => {
                    // consume an attempt so persistent errors stay bounded by max_reposts
                    f.repost_count += 1;
                    f.next_attempt_at = now + self.cfg.propagation_delay;
                    f.last_error = format!("repost failed: {e}");
                    let _ = self.store.update_failure_after_check(&f);
                    return;
                }
            }
        }
        self.defer_or_fail(f, now);
    }

    /// Schedules the next STAT-only recheck with backoff, or permanently fails the failure once
    /// rechecks are exhausted.
    fn defer_or_fail(&self, mut f: VerificationFailure, now: DateTime<Utc>) {
        f.deferred_count += 1;
        if f.deferred_count >= self.cfg.max_deferred_checks {
            f.state = ts::FAILURE_FAILED.into();
            f.next_attempt_at = now;
            f.last_error = "exhausted deferred rechecks".into();
        } else {
            f.state = ts::FAILURE_PENDING.into();
            f.next_attempt_at = now + self.backoff(f.deferred_count - 1);
            f.last_error = "article still missing".into();
        }
        let _ = self.store.update_failure_after_check(&f);
    }

    /// Finalizes completed items stuck in `pending_verification` that the normal flow cannot reach:
    /// terminal-but-never-finalized transfers, crash orphans and pre-durable legacy items. Items with
    /// live files or pending legacy failures are left alone.
    pub async fn reconcile_stuck(&self, cancel: &CancellationToken) -> Result<()> {
        let ids = self.store.list_pending_verification_item_ids()?;
        let mut repaired = 0;
        for id in ids {
            if cancel.is_cancelled() {
                return Err(VerifyError::Canceled);
            }
            let files = match self.store.list_files_by_completed_item(&id) {
                Ok(f) => f,
                Err(e) => {
                    log(&format!("reconcile list files failed: item={id} error={e}"));
                    continue;
                }
            };
            if let Some(first) = files.first() {
                // no-op unless every file is terminal, so live transfers are untouched
                self.finalize_transfer(&first.transfer_id).await;
                continue;
            }
            if self.reconcile_legacy_item(&id) {
                repaired += 1;
            }
        }
        if repaired > 0 {
            log(&format!(
                "repaired stuck pending_verification items: count={repaired}"
            ));
        }
        Ok(())
    }

    /// Resolves an item with no linked transfer files from its legacy STAT-only failures: outstanding
    /// work leaves it alone; otherwise verified, or failed when any legacy failure exhausted retries.
    fn reconcile_legacy_item(&self, completed_item_id: &str) -> bool {
        let count = |state: &str| {
            self.store
                .count_failures(completed_item_id, ts::LEGACY_FILE_ID, state)
        };
        let Ok(outstanding) = count(ts::FAILURE_PENDING) else {
            return false;
        };
        if outstanding > 0 {
            return false;
        }
        let Ok(failed) = count(ts::FAILURE_FAILED) else {
            return false;
        };
        let status = if failed > 0 {
            STATUS_FAILED
        } else {
            STATUS_VERIFIED
        };
        self.store
            .set_completed_item_verification_status(completed_item_id, status)
            .is_ok()
    }

    /// Verified when no pending failures remain, `verification_failed` when only permanent ones do.
    async fn reconcile_file_state(&self, transfer_id: &str, file_id: &str) {
        if file_id == ts::LEGACY_FILE_ID {
            // legacy failures have no transfer_files row; resolve the completed item directly
            self.reconcile_legacy_item(transfer_id);
            return;
        }
        let Ok(outstanding) = self
            .store
            .count_failures(transfer_id, file_id, ts::FAILURE_PENDING)
        else {
            return;
        };
        if outstanding > 0 {
            return;
        }
        let Ok(failed) = self
            .store
            .count_failures(transfer_id, file_id, ts::FAILURE_FAILED)
        else {
            return;
        };
        if failed > 0 {
            let _ = self.store.set_verification_state(
                transfer_id,
                file_id,
                ts::STATE_VERIFICATION_FAILED,
                None,
                "verification failed for some articles",
            );
        } else {
            let _ = self.store.set_verification_state(
                transfer_id,
                file_id,
                ts::STATE_VERIFIED,
                None,
                "",
            );
        }
        self.finalize_transfer(transfer_id).await;
    }

    /// Streams the manifest of each file with a re-postable failure, at most once per batch.
    fn resolve_records(
        &self,
        failures: &[VerificationFailure],
    ) -> HashMap<RecordKey, ArticleRecord> {
        let mut needed: HashMap<(String, String), HashSet<i64>> = HashMap::new();
        for f in failures {
            if f.repost_count >= self.cfg.max_reposts || f.article_index < 0 {
                continue;
            }
            needed
                .entry((f.transfer_id.clone(), f.file_id.clone()))
                .or_default()
                .insert(f.article_index);
        }
        let mut out = HashMap::new();
        for ((transfer, file), indices) in needed {
            let tf = match self.store.get_file(&transfer, &file) {
                Ok(Some(tf)) => tf,
                Ok(None) => continue,
                Err(e) => {
                    log(&format!(
                        "load transfer file failed: transfer={transfer} file={file} error={e}"
                    ));
                    continue;
                }
            };
            let mut r = match Reader::open(std::path::Path::new(&tf.manifest_path)) {
                Ok(r) => r,
                Err(e) => {
                    log(&format!(
                        "open manifest failed: path={} error={e}",
                        tf.manifest_path
                    ));
                    continue;
                }
            };
            let mut remaining = indices.len();
            while remaining > 0 {
                let Ok(Some(rec)) = r.next_record() else {
                    break;
                };
                if indices.contains(&rec.index) {
                    out.insert(RecordKey(transfer.clone(), file.clone(), rec.index), rec);
                    remaining -= 1;
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests;
