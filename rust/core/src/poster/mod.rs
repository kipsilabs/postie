//! Port of `internal/poster`: posts files to Usenet.
//!
//! Per file: plan the articles ([`plan`]), record a manifest before any network traffic (durable
//! mode) or recover from an existing one by re-posting only the articles the server lacks, then read
//! each article's body ahead of time under the engine's memory budget and post it through the shared
//! upload client under the engine's worker limit. Without a manifest sink the legacy immediate check
//! runs: STAT every article, re-post the failures up to `max_reposts` times, and defer what still
//! fails. Go's goroutines/WaitGroups/channels become tasks under one cancellation token; the first
//! fatal error cancels the run.

pub mod plan;
mod reposter;

use std::collections::HashMap;
use std::fs::File;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use postie_nntp::NntpError;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::article::Article;
use crate::config::{PostCheck, PostingConfig};
use crate::engine::{BufferPermit, Engine};
use crate::manifest::ArticleRecord;
use crate::nzb::Generator;
use crate::pausable::PauseGate;
use crate::progress::{JobProgress, Progress, ProgressType};
use crate::throttle::Throttle;
use crate::uploadmeter::UploadMeter;
pub use reposter::{Reposter, article_from_record};

pub type BoxFut<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// The NNTP operations the poster needs (a seam for tests and for the pool manager).
pub trait ArticleClient: Send + Sync {
    /// Posts a complete article (headers, blank line, yEnc body).
    fn post(&self, article: Vec<u8>) -> BoxFut<'_, Result<(), NntpError>>;
    /// `Ok(true)` when present, `Ok(false)` when missing.
    fn stat<'a>(&'a self, message_id: &'a str) -> BoxFut<'a, Result<bool, NntpError>>;
    /// Total connection capacity (sizes the shared worker pool).
    fn max_connections(&self) -> usize;
}

impl ArticleClient for postie_nntp::pool::Client {
    fn post(&self, article: Vec<u8>) -> BoxFut<'_, Result<(), NntpError>> {
        Box::pin(postie_nntp::pool::Client::post(self, article))
    }
    fn stat<'a>(&'a self, message_id: &'a str) -> BoxFut<'a, Result<bool, NntpError>> {
        Box::pin(postie_nntp::pool::Client::stat(self, message_id))
    }
    fn max_connections(&self) -> usize {
        postie_nntp::pool::Client::max_connections(self)
    }
}

/// Records a durable manifest before a file's articles are posted and returns an earlier one for crash
/// recovery. Implemented by the transfer runtime; absent in standalone mode.
pub trait ManifestSink: Send + Sync {
    fn record_file<'a>(
        &'a self,
        file_path: &'a Path,
        articles: &'a [Article],
    ) -> BoxFut<'a, Result<(), String>>;
    fn existing_articles<'a>(
        &'a self,
        file_path: &'a Path,
    ) -> BoxFut<'a, Result<Option<Vec<ArticleRecord>>, String>>;
}

/// An article that exhausted its immediate re-posts and is left for deferred verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedArticleInfo {
    pub message_id: String,
    pub groups: Vec<String>,
}

/// Non-fatal: the upload succeeded but some articles still need deferred STAT verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeferredCheckError {
    pub failed_articles: Vec<FailedArticleInfo>,
    pub total_articles: usize,
}

/// Error texts match the Go messages.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PostError {
    #[error("poster is closed")]
    Closed,
    #[error("context canceled")]
    Canceled,
    #[error("error adding file {file} to posting queue: {cause}")]
    Add { file: String, cause: String },
    #[error("failed to post file {file} after {retries} retries: {cause}")]
    File {
        file: String,
        retries: i64,
        cause: String,
    },
    #[error("failed to verify file {file} after {retries} retries")]
    Verify { file: String, retries: u64 },
    #[error("failed to post {0} files")]
    FilesFailed(u64),
    #[error("{}/{} articles deferred for later verification", .0.failed_articles.len(), .0.total_articles)]
    Deferred(DeferredCheckError),
    /// The re-post source file no longer exists (re-posting can never succeed).
    #[error("{0}")]
    SourceMissing(String),
    #[error("{0}")]
    Other(String),
}

/// Counters for posted and checked articles.
#[derive(Default)]
pub struct Stats {
    pub articles_posted: AtomicU64,
    pub articles_checked: AtomicU64,
    pub bytes_posted: AtomicU64,
    pub article_errors: AtomicU64,
    start: Mutex<Option<Instant>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StatsSnapshot {
    pub articles_posted: u64,
    pub articles_checked: u64,
    pub bytes_posted: u64,
    pub article_errors: u64,
}

impl Stats {
    pub fn new() -> Self {
        let s = Self::default();
        *s.start.lock().unwrap_or_else(|p| p.into_inner()) = Some(Instant::now());
        s
    }
    pub fn snapshot(&self) -> StatsSnapshot {
        StatsSnapshot {
            articles_posted: self.articles_posted.load(Ordering::Relaxed),
            articles_checked: self.articles_checked.load(Ordering::Relaxed),
            bytes_posted: self.bytes_posted.load(Ordering::Relaxed),
            article_errors: self.article_errors.load(Ordering::Relaxed),
        }
    }
}

/// A single read that takes longer than this fails the post instead of hanging on a dead mount.
const READ_STALL_TIMEOUT: Duration = Duration::from_secs(60);
/// Articles a single file may have read ahead or in flight.
const READ_AHEAD: usize = 50;
/// Reads allowed in flight across every job at once, for HDD friendliness only. The permit is held by
/// the caller and released when the caller returns (success, cancel or stall timeout), so a stalled
/// read never keeps a global slot: one dead mount cannot starve reads from healthy disks.
const MAX_CONCURRENT_READS: usize = 8;
static READ_SLOTS: std::sync::LazyLock<Arc<Semaphore>> =
    std::sync::LazyLock::new(|| Arc::new(Semaphore::new(MAX_CONCURRENT_READS)));
/// Blocking reads one file may have outstanding in the OS. Its permit travels into the blocking
/// thread and is freed only when the OS read returns, so a dead mount strands at most this many
/// threads per file, and only that file's reads stall.
const MAX_READS_PER_FILE: usize = 4;

/// Per-file read slots; create one per source file and share it by clone.
pub(crate) fn new_file_read_slots() -> Arc<Semaphore> {
    Arc::new(Semaphore::new(MAX_READS_PER_FILE))
}
/// Large files processed concurrently by one `post` call (overlaps one file's tail with the next's
/// head). Small files share the same budget by article count, so many of them run at once.
const POST_PIPELINE_DEPTH: usize = 2;

/// Removes a multi-file post's overall row when the post ends (success, error or cancel).
struct AggregateRow(Arc<JobProgress>, Uuid);

impl Drop for AggregateRow {
    fn drop(&mut self) {
        self.0.finish_progress(self.1);
    }
}

/// Pipeline slots for one `post` call: one per upload connection (at least the large-file depth).
fn pipeline_capacity(connections: usize) -> usize {
    connections.max(POST_PIPELINE_DEPTH)
}

/// Slots a file holds while it posts: one per article, capped so `POST_PIPELINE_DEPTH` large files
/// still overlap. A single-article file takes one slot, so a folder of tiny files fills the pool
/// instead of waiting on two round trips at a time.
fn pipeline_slots(articles: usize, capacity: usize) -> u32 {
    let cap = (capacity / POST_PIPELINE_DEPTH).max(1);
    u32::try_from(articles.clamp(1, cap)).unwrap_or(u32::MAX)
}
/// Posts per article on a stale connection.
const POST_ATTEMPTS: usize = 3;

#[cfg(unix)]
fn read_exact_at(f: &File, buf: &mut [u8], off: u64) -> std::io::Result<()> {
    std::os::unix::fs::FileExt::read_exact_at(f, buf, off)
}

#[cfg(windows)]
fn read_exact_at(f: &File, mut buf: &mut [u8], mut off: u64) -> std::io::Result<()> {
    while !buf.is_empty() {
        let n = std::os::windows::fs::FileExt::seek_read(f, buf, off)?;
        if n == 0 {
            return Err(std::io::ErrorKind::UnexpectedEof.into());
        }
        buf = &mut buf[n..];
        off += n as u64;
    }
    Ok(())
}

/// Largest single article body ever read from disk. Real bodies are under a few MiB; the cap keeps
/// a corrupt manifest record from turning into a multi-GB allocation that aborts the process.
pub(crate) const MAX_ARTICLE_READ: usize = 64 * 1024 * 1024;

/// Checks a manifest-supplied `(offset, size)` against the source file and the read cap, returning
/// them as unsigned values. Rejects negatives and ranges that run past the end of the file.
pub(crate) fn validate_read_range(
    file: &File,
    offset: i64,
    size: u64,
) -> Result<(u64, usize), String> {
    let (off, len) = match (u64::try_from(offset), usize::try_from(size)) {
        (Ok(o), Ok(l)) if l > 0 && l <= MAX_ARTICLE_READ => (o, l),
        _ => {
            return Err(format!(
                "invalid article range: offset {offset}, size {size}"
            ));
        }
    };
    let file_len = file.metadata().map_err(|e| e.to_string())?.len();
    match off.checked_add(len as u64) {
        Some(end) if end <= file_len => Ok((off, len)),
        _ => Err(format!(
            "article range {off}+{len} is past the end of the {file_len}-byte source file"
        )),
    }
}

/// Reads `len` bytes at `off` without ever hanging the caller: cancellation and a stall deadline both
/// return (the blocked read itself ends when the OS releases it). `file_slots` isolates stalls per file.
pub(crate) async fn read_at_with_stall_guard(
    file: &Arc<File>,
    file_slots: &Arc<Semaphore>,
    off: u64,
    len: usize,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, String> {
    let f = file.clone();
    guarded_read(
        &READ_SLOTS,
        file_slots,
        off,
        len,
        cancel,
        READ_STALL_TIMEOUT,
        move || {
            let mut buf = vec![0u8; len];
            read_exact_at(&f, &mut buf, off).map(|()| buf)
        },
    )
    .await
}

/// Slot logic behind [`read_at_with_stall_guard`], with the blocking read injectable for tests.
async fn guarded_read<F>(
    global: &Arc<Semaphore>,
    file_slots: &Arc<Semaphore>,
    off: u64,
    len: usize,
    cancel: &CancellationToken,
    stall: Duration,
    read: F,
) -> Result<Vec<u8>, String>
where
    F: FnOnce() -> std::io::Result<Vec<u8>> + Send + 'static,
{
    if len > MAX_ARTICLE_READ {
        return Err(format!("refusing to read {len} bytes in one article"));
    }
    let stalled =
        || format!("read stalled after {stall:?} at offset {off} (filesystem unresponsive?)");
    let work = async {
        // Per-file permit first: it moves into the blocking read (freed when the OS read returns).
        let file_permit = file_slots
            .clone()
            .acquire_owned()
            .await
            .map_err(|e| e.to_string())?;
        // Global permit stays with this future and drops when we return, stalled or not.
        let _global = global.acquire().await.map_err(|e| e.to_string())?;
        let task = tokio::task::spawn_blocking(move || {
            let _slot = file_permit;
            read()
        });
        match task.await {
            Ok(Ok(buf)) => Ok(buf),
            Ok(Err(e)) => Err(e.to_string()),
            Err(e) => Err(e.to_string()),
        }
    };
    tokio::select! {
        () = cancel.cancelled() => Err("context canceled".into()),
        r = tokio::time::timeout(stall, work) => r.unwrap_or_else(|_| Err(stalled())),
    }
}

/// Builds the wire form of `art` around `body` and posts it, retrying on stale pooled connections.
/// Shared by uploads and by the durable re-poster so both produce identical headers.
/// Minimum gap between two post-failure lines in `postie.log`; failures in between are counted.
const POST_FAILURE_LOG_GAP: Duration = Duration::from_secs(1);

/// Rate limiter for [`log_post_failure`]: `Some(n)` when a line may be written now, `n` being the
/// failures suppressed since the previous line.
#[derive(Default)]
struct FailureLogGate {
    last: Option<Instant>,
    suppressed: u64,
}

impl FailureLogGate {
    fn admit(&mut self, now: Instant) -> Option<u64> {
        if self
            .last
            .is_some_and(|t| now.duration_since(t) < POST_FAILURE_LOG_GAP)
        {
            self.suppressed += 1;
            return None;
        }
        self.last = Some(now);
        Some(std::mem::take(&mut self.suppressed))
    }
}

/// Records a provider-side post failure in `postie.log` (the desktop app has no visible stderr, so
/// without this a provider refusing connections leaves an upload silently stalled).
fn log_post_failure(message_id: &str, err: &NntpError) {
    static GATE: std::sync::OnceLock<Mutex<FailureLogGate>> = std::sync::OnceLock::new();
    let admitted = GATE
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .admit(Instant::now());
    let Some(suppressed) = admitted else { return };
    crate::logx::warn(
        "Article post failed",
        &[
            ("messageID", message_id.into()),
            ("error", err.to_string().into()),
            ("suppressedSinceLast", suppressed.into()),
        ],
    );
}

pub(crate) async fn post_yenc(
    cancel: &CancellationToken,
    client: &dyn ArticleClient,
    throttle: Option<&Throttle>,
    stats: Option<&Stats>,
    meter: Option<&UploadMeter>,
    art: &Article,
    body: &[u8],
) -> Result<(), PostError> {
    // pace BEFORE sending so the bucket bounds egress rather than lagging one article behind it
    if let Some(t) = throttle {
        t.wait(art.size as i64).await;
    }
    let mut last: Option<NntpError> = None;
    for attempt in 0..POST_ATTEMPTS {
        if attempt > 0 {
            eprintln!(
                "Retrying article post after stale pooled connection: messageID={} attempt={attempt}",
                art.message_id
            );
        }
        let wire = art.encode(body).map_err(PostError::Other)?;
        let res = tokio::select! {
            () = cancel.cancelled() => return Err(PostError::Canceled),
            r = tokio::time::timeout(crate::timeouts::current().post, client.post(wire)) => r,
        };
        match res {
            Err(_) => {
                return Err(PostError::Other(
                    "posting article timed out: context deadline exceeded".into(),
                ));
            }
            Ok(Ok(())) => {
                if let Some(m) = meter {
                    m.record(body.len() as i64);
                }
                last = None;
                break;
            }
            Ok(Err(e)) => {
                let retry = e.is_retryable() && !matches!(e, NntpError::Connect(_));
                last = Some(e);
                if !retry {
                    break;
                }
            }
        }
    }
    if let Some(e) = last {
        log_post_failure(&art.message_id, &e);
        return Err(PostError::Other(format!("error posting article: {e}")));
    }
    if let Some(s) = stats {
        s.articles_posted.fetch_add(1, Ordering::Relaxed);
        s.bytes_posted.fetch_add(art.size, Ordering::Relaxed);
    }
    Ok(())
}

struct Inner {
    cfg: PostingConfig,
    check: PostCheck,
    upload: Arc<dyn ArticleClient>,
    verify: Arc<dyn ArticleClient>,
    stats: Stats,
    throttle: Option<Arc<Throttle>>,
    progress: Arc<JobProgress>,
    engine: Option<Arc<Engine>>,
    manifest_sink: Option<Arc<dyn ManifestSink>>,
    /// Shared upload workers: total posting concurrency is bounded by upload connection capacity no
    /// matter how many `post` calls run at once.
    workers: Arc<Semaphore>,
    closed: AtomicBool,
    shutdown: CancellationToken,
}

/// Posts files. Cheap to clone (shared state).
#[derive(Clone)]
pub struct Poster(Arc<Inner>);

/// A file ready to be posted.
struct Prepared {
    path: PathBuf,
    file: Arc<File>,
    read_slots: Arc<Semaphore>,
    size: i64,
    articles: Vec<Article>,
}

/// State shared by the files of one `post` call.
#[derive(Default)]
struct Run {
    fatal: Mutex<Option<PostError>>,
    failed_files: AtomicU64,
    deferred: Mutex<Vec<FailedArticleInfo>>,
    total_articles: AtomicUsize,
    first_check_done: AtomicBool,
    /// One overall row for a multi-file post (bytes across all files).
    aggregate: Option<Arc<Progress>>,
}

impl Run {
    fn set_fatal(&self, e: PostError) {
        let mut f = self.fatal.lock().unwrap_or_else(|p| p.into_inner());
        f.get_or_insert(e);
    }
    fn has_fatal(&self) -> bool {
        self.fatal
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
    }
}

fn base_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

impl Poster {
    /// `verify` may be the same client as `upload` when no verify-role servers are configured.
    pub fn new(
        cfg: PostingConfig,
        check: PostCheck,
        upload: Arc<dyn ArticleClient>,
        verify: Arc<dyn ArticleClient>,
        progress: Arc<JobProgress>,
        engine: Option<Arc<Engine>>,
        manifest_sink: Option<Arc<dyn ManifestSink>>,
    ) -> Self {
        let throttle = match &engine {
            Some(e) => e.shared_throttle(cfg.throttle_rate),
            None => (cfg.throttle_rate > 0).then(|| Arc::new(Throttle::new(cfg.throttle_rate))),
        };
        let conns = upload.max_connections().max(1);
        Self(Arc::new(Inner {
            cfg,
            check,
            upload,
            verify,
            stats: Stats::new(),
            throttle,
            progress,
            engine,
            manifest_sink,
            workers: Arc::new(Semaphore::new(conns)),
            closed: AtomicBool::new(false),
            shutdown: CancellationToken::new(),
        }))
    }

    pub fn stats(&self) -> StatsSnapshot {
        self.0.stats.snapshot()
    }

    /// No further `post` calls are accepted and in-flight work is cancelled.
    pub fn close(&self) {
        self.0.closed.store(true, Ordering::SeqCst);
        self.0.shutdown.cancel();
    }

    /// The legacy in-poster check runs only without a manifest sink (durable mode verifies in the
    /// background service, which frees the upload slot before propagation delay).
    fn immediate_check_enabled(&self) -> bool {
        self.0.manifest_sink.is_none() && self.0.check.enabled == Some(true)
    }

    pub async fn post(
        &self,
        cancel: &CancellationToken,
        files: &[PathBuf],
        nzb: &Arc<Generator>,
        pause: Option<&PauseGate>,
    ) -> Result<(), PostError> {
        self.post_with_relative_paths(cancel, files, nzb, &HashMap::new(), pause)
            .await
    }

    /// Posts `files`; `relative_paths` maps a file to the display name used in its subject.
    /// The overall row for a multi-file post, sized to the files' total bytes; `None` for one file.
    fn aggregate_progress(&self, files: &[PathBuf]) -> Option<Arc<Progress>> {
        if files.len() < 2 {
            return None;
        }
        // one stat per file: blocking I/O, kept off the async workers
        let total: i64 = crate::blocking::in_place(|| {
            files
                .iter()
                .filter_map(|f| std::fs::metadata(f).ok())
                .map(|m| i64::try_from(m.len()).unwrap_or(i64::MAX))
                .sum()
        });
        Some(self.0.progress.add_progress(
            Uuid::new_v4(),
            &format!("Uploading {} files", files.len()),
            ProgressType::Uploading,
            total,
        ))
    }

    pub async fn post_with_relative_paths(
        &self,
        cancel: &CancellationToken,
        files: &[PathBuf],
        nzb: &Arc<Generator>,
        relative_paths: &HashMap<String, String>,
        pause: Option<&PauseGate>,
    ) -> Result<(), PostError> {
        if self.0.closed.load(Ordering::SeqCst) {
            return Err(PostError::Closed);
        }
        let ctx = self.0.shutdown.child_token();
        let forward = {
            let (outer, ctx) = (cancel.clone(), ctx.clone());
            tokio::spawn(async move {
                tokio::select! {
                    () = outer.cancelled() => ctx.cancel(),
                    () = ctx.cancelled() => {}
                }
            })
        };
        let aggregate = self.aggregate_progress(files);
        let run = Arc::new(Run {
            aggregate: aggregate.clone(),
            ..Run::default()
        });
        // the overall row goes away however the post ends
        let _aggregate_row = aggregate.map(|a| AggregateRow(self.0.progress.clone(), a.id()));
        let capacity = pipeline_capacity(self.0.upload.max_connections());
        let depth = Arc::new(Semaphore::new(capacity));
        let mut tasks: JoinSet<()> = JoinSet::new();
        let mut early: Option<PostError> = None;

        for (i, file) in files.iter().enumerate() {
            if ctx.is_cancelled() {
                early = Some(PostError::Canceled);
                break;
            }
            if run.has_fatal() {
                break;
            }
            let display = relative_paths
                .get(&file.to_string_lossy().into_owned())
                .cloned()
                .unwrap_or_default();
            let prepared = match self
                .prepare(&ctx, file, &display, i as i64 + 1, files.len() as i64, nzb)
                .await
            {
                Ok(Some(p)) => p,
                Ok(None) => continue,
                Err(e) => {
                    early = Some(match e {
                        PostError::Canceled => PostError::Canceled,
                        other => PostError::Add {
                            file: file.to_string_lossy().into_owned(),
                            cause: other.to_string(),
                        },
                    });
                    break;
                }
            };
            let permit = tokio::select! {
                () = ctx.cancelled() => { early = Some(PostError::Canceled); break; }
                p = depth.clone().acquire_many_owned(pipeline_slots(prepared.articles.len(), capacity)) => p.ok(),
            };
            let (me, ctx2, run2, nzb2, pause2) = (
                self.clone(),
                ctx.clone(),
                run.clone(),
                nzb.clone(),
                pause.cloned(),
            );
            tasks.spawn(async move {
                let _permit = permit;
                me.run_file(&ctx2, &run2, prepared, &nzb2, pause2.as_ref())
                    .await;
            });
        }
        if early.is_some() {
            ctx.cancel();
        }
        while tasks.join_next().await.is_some() {}
        ctx.cancel();
        let _ = forward.await;

        if let Some(e) = run.fatal.lock().unwrap_or_else(|p| p.into_inner()).take() {
            return Err(e);
        }
        if let Some(e) = early {
            return Err(
                if e == PostError::Canceled && self.0.closed.load(Ordering::SeqCst) {
                    PostError::Closed
                } else {
                    e
                },
            );
        }
        if cancel.is_cancelled() {
            return Err(PostError::Canceled);
        }
        let n = run.failed_files.load(Ordering::Relaxed);
        if n > 0 {
            return Err(PostError::FilesFailed(n));
        }
        let deferred = std::mem::take(&mut *run.deferred.lock().unwrap_or_else(|p| p.into_inner()));
        if !deferred.is_empty() {
            return Err(PostError::Deferred(DeferredCheckError {
                failed_articles: deferred,
                total_articles: run.total_articles.load(Ordering::Relaxed),
            }));
        }
        Ok(())
    }

    /// Opens, plans and (durable mode) records one file. `None` for an empty file, which has nothing
    /// to post.
    async fn prepare(
        &self,
        ctx: &CancellationToken,
        path: &Path,
        display: &str,
        file_number: i64,
        total_files: i64,
        nzb: &Arc<Generator>,
    ) -> Result<Option<Prepared>, PostError> {
        let (file, size) = crate::blocking::in_place(|| {
            let file = File::open(path)
                .map_err(|e| PostError::Other(format!("error opening file: {e}")))?;
            let size = file
                .metadata()
                .map_err(|e| PostError::Other(format!("error getting file info: {e}")))?
                .len() as i64;
            Ok::<_, PostError>((file, size))
        })?;
        if size == 0 {
            eprintln!("Skipping empty file: {}", path.display());
            return Ok(None);
        }
        let file = Arc::new(file);
        // Crash recovery: reuse an existing manifest's exact Message-IDs and re-post only the
        // articles still missing on the server.
        if let Some(sink) = &self.0.manifest_sink {
            match sink.existing_articles(path).await {
                Err(e) => eprintln!(
                    "Failed to read existing manifest; regenerating: file={} error={e}",
                    path.display()
                ),
                Ok(Some(recs)) => {
                    return Ok(Some(
                        self.recovered(ctx, path, file_number, file, size, &recs, nzb)
                            .await,
                    ));
                }
                Ok(None) => {}
            }
        }
        let plan = plan::FilePlan {
            path,
            size,
            display_name: display,
            file_number,
            total_files,
        };
        let articles = plan::plan_articles(&self.0.cfg, &plan, &mut rand::rng())
            .map_err(|e| PostError::Other(e.to_string()))?;
        // Record the manifest BEFORE any network posting so an interrupted upload can be resumed with
        // the exact Message-IDs; a file whose manifest cannot be written is failed, not posted blind.
        if let Some(sink) = &self.0.manifest_sink {
            sink.record_file(path, &articles).await.map_err(|e| {
                PostError::Other(format!("recording manifest for {}: {e}", path.display()))
            })?;
        }
        Ok(Some(Prepared {
            path: path.to_path_buf(),
            file,
            read_slots: new_file_read_slots(),
            size,
            articles,
        }))
    }

    /// Reconstructs articles from a manifest and keeps only those the server lacks. Articles already
    /// on the server are not re-posted but still belong in the NZB.
    #[allow(clippy::too_many_arguments)]
    async fn recovered(
        &self,
        ctx: &CancellationToken,
        path: &Path,
        file_number: i64,
        file: Arc<File>,
        size: i64,
        recs: &[ArticleRecord],
        nzb: &Arc<Generator>,
    ) -> Prepared {
        let original = base_name(path);
        let all: Vec<Article> = recs
            .iter()
            .map(|r| {
                let mut a = article_from_record(r);
                a.original_name = original.clone();
                a.file_number = file_number;
                a
            })
            .collect();
        // The sweep STATs every recorded article and can take minutes for a large file: show it,
        // or the job looks stuck (running, no progress, no upload traffic) until posting resumes.
        let check = self.0.progress.add_progress(
            Uuid::new_v4(),
            &format!("{original} (resume check)"),
            ProgressType::Checking,
            size,
        );
        let missing = self.filter_missing(ctx, &all, Some(&check)).await;
        self.0.progress.finish_progress(check.id());
        let missing_ids: std::collections::HashSet<&str> =
            missing.iter().map(|a| a.message_id.as_str()).collect();
        for a in all
            .iter()
            .filter(|a| !missing_ids.contains(a.message_id.as_str()))
        {
            nzb.add_article(a.clone());
        }
        eprintln!(
            "Recovered manifest; re-posting only missing articles: file={} total={} missing={}",
            path.display(),
            all.len(),
            missing.len()
        );
        Prepared {
            path: path.to_path_buf(),
            file,
            read_slots: new_file_read_slots(),
            size,
            articles: missing,
        }
    }

    /// STATs articles in batches and returns those NOT confirmed present, in order. A STAT error
    /// counts as missing (re-posting a present article is idempotent).
    async fn filter_missing(
        &self,
        ctx: &CancellationToken,
        articles: &[Article],
        progress: Option<&Progress>,
    ) -> Vec<Article> {
        if articles.is_empty() {
            return vec![];
        }
        let batch = if self.0.check.stat_batch_size > 0 {
            self.0.check.stat_batch_size as usize
        } else {
            100
        };
        let mut missing = Vec::new();
        for chunk in articles.chunks(batch) {
            let mut set: JoinSet<(usize, bool)> = JoinSet::new();
            for (i, a) in chunk.iter().enumerate() {
                let (me, id) = (self.clone(), a.message_id.clone());
                set.spawn(async move { (i, matches!(me.0.verify.stat(&id).await, Ok(true))) });
            }
            let mut present = vec![false; chunk.len()];
            while let Some(Ok((i, p))) = set.join_next().await {
                present[i] = p;
            }
            if ctx.is_cancelled() {
                return articles.to_vec(); // a cancelled sweep falls back to re-posting everything
            }
            if let Some(p) = progress {
                p.update_progress(chunk.iter().map(|a| a.size as i64).sum());
            }
            missing.extend(
                chunk
                    .iter()
                    .zip(present)
                    .filter(|(_, p)| !p)
                    .map(|(a, _)| a.clone()),
            );
        }
        missing
    }

    /// Posts one file (and, in immediate-check mode, verifies and re-posts it).
    async fn run_file(
        &self,
        ctx: &CancellationToken,
        run: &Arc<Run>,
        prep: Prepared,
        nzb: &Arc<Generator>,
        pause: Option<&PauseGate>,
    ) {
        let name = base_name(&prep.path);
        let mut todo = prep.articles;
        let mut retries: u64 = 0;
        let mut label = name.clone();
        loop {
            // a single-article file finishes faster than the dashboard refreshes: in a multi-file post
            // the overall row reports it instead of a row that flashes at 0%
            let progress = if run.aggregate.is_some() && todo.len() <= 1 {
                self.0
                    .progress
                    .detached(&label, ProgressType::Uploading, prep.size)
            } else {
                self.0.progress.add_progress(
                    Uuid::new_v4(),
                    &label,
                    ProgressType::Uploading,
                    prep.size,
                )
            };
            let (completed, err) = self
                .post_articles(
                    ctx,
                    &prep.file,
                    &prep.read_slots,
                    &prep.path,
                    &todo,
                    &progress,
                    run.aggregate.as_ref(),
                    pause,
                )
                .await;
            for a in &completed {
                nzb.add_article(a.clone());
            }
            self.0.progress.finish_progress(progress.id());
            if let Some(e) = err {
                if e != PostError::Canceled {
                    run.set_fatal(PostError::File {
                        file: prep.path.to_string_lossy().into_owned(),
                        retries: self.0.cfg.max_retries,
                        cause: e.to_string(),
                    });
                }
                ctx.cancel();
                return;
            }
            if !self.immediate_check_enabled() {
                return;
            }
            // ----- legacy immediate check -----
            let check_progress = self.0.progress.add_progress(
                Uuid::new_v4(),
                &format!("{name} (check)"),
                ProgressType::Checking,
                prep.size,
            );
            // propagation delay only before the first file's first check
            if !run.first_check_done.swap(true, Ordering::SeqCst) {
                let delay = self.0.check.retry_delay.nanos();
                if delay > 0 {
                    let d = Duration::from_nanos(delay as u64);
                    check_progress.set_wait_deadline(Some(Instant::now() + d));
                    let cancelled = tokio::select! { () = ctx.cancelled() => true, () = tokio::time::sleep(d) => false };
                    check_progress.set_wait_deadline(None);
                    if cancelled {
                        self.0.progress.finish_progress(check_progress.id());
                        return;
                    }
                }
            }
            run.total_articles
                .fetch_add(completed.len(), Ordering::Relaxed);
            let failed = self
                .check_articles(ctx, &completed, &check_progress, pause)
                .await;
            self.0.progress.finish_progress(check_progress.id());
            if ctx.is_cancelled() {
                return;
            }
            if failed.is_empty() {
                return;
            }
            retries += 1;
            if retries < self.0.check.max_repost {
                // The Message-ID is preserved on purpose: the NZB keys segments by (file, id), so a new
                // id would append a duplicate segment instead of replacing the entry.
                todo = failed
                    .into_iter()
                    .map(|mut a| {
                        if a.from.is_empty() {
                            a.from = if self.0.cfg.post_headers.default_from.is_empty() {
                                plan_random_from()
                            } else {
                                self.0.cfg.post_headers.default_from.clone()
                            };
                        }
                        if a.subject.is_empty() {
                            a.subject = plan_random_subject();
                        }
                        a
                    })
                    .collect();
                label = format!("{name} (retry)");
                continue;
            }
            if self.0.check.deferred_check_delay.nanos() > 0 {
                run.deferred
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .extend(failed.iter().map(|a| FailedArticleInfo {
                        message_id: a.message_id.clone(),
                        groups: a.groups.clone(),
                    }));
                return; // optimistically verified; the deferred check updates it later
            }
            run.failed_files.fetch_add(1, Ordering::Relaxed);
            run.set_fatal(PostError::Verify {
                file: prep.path.to_string_lossy().into_owned(),
                retries: self.0.check.max_repost,
            });
            ctx.cancel();
            return;
        }
    }

    /// Posts `articles` of one file: bodies are read ahead (bounded by `READ_AHEAD` and the engine's
    /// buffer budget) and posted under the shared worker pool. Returns the articles that were posted
    /// and the first error; once an article fails no further articles of this file are scheduled
    /// (those already in flight finish).
    #[allow(clippy::too_many_arguments)]
    async fn post_articles(
        &self,
        ctx: &CancellationToken,
        file: &Arc<File>,
        file_slots: &Arc<Semaphore>,
        path: &Path,
        articles: &[Article],
        progress: &Arc<Progress>,
        aggregate: Option<&Arc<Progress>>,
        pause: Option<&PauseGate>,
    ) -> (Vec<Article>, Option<PostError>) {
        let reserve = self.0.engine.as_ref().map_or(0, |e| e.per_article_bytes());
        let window = Arc::new(Semaphore::new(READ_AHEAD));
        let mut set: JoinSet<Result<Article, PostError>> = JoinSet::new();
        let mut first_err: Option<PostError> = None;
        let mut completed = Vec::with_capacity(articles.len());

        let collect = |r: Option<Result<Result<Article, PostError>, tokio::task::JoinError>>,
                       first_err: &mut Option<PostError>,
                       completed: &mut Vec<Article>| {
            match r {
                Some(Ok(Ok(a))) => {
                    progress.update_progress(a.size as i64);
                    if let Some(all) = aggregate {
                        all.update_progress(a.size as i64);
                    }
                    completed.push(a);
                }
                Some(Ok(Err(e))) => {
                    first_err.get_or_insert(e);
                }
                Some(Err(e)) => {
                    first_err.get_or_insert(PostError::Other(e.to_string()));
                }
                None => {}
            }
        };

        'articles: for art in articles {
            // drain finished tasks so the window frees and errors surface early
            while let Some(r) = set.try_join_next() {
                collect(Some(r), &mut first_err, &mut completed);
            }
            if ctx.is_cancelled() {
                first_err.get_or_insert(PostError::Canceled);
                break;
            }
            if first_err.is_some() {
                // the file is already doomed; posting the rest would only waste bandwidth (a retry
                // re-posts just the missing articles from the manifest)
                break;
            }
            let win = tokio::select! {
                () = ctx.cancelled() => { first_err.get_or_insert(PostError::Canceled); break 'articles; }
                w = window.clone().acquire_owned() => w.ok(),
            };
            let buffer: Option<BufferPermit> = match &self.0.engine {
                Some(e) => match e.reserve_buffer(ctx, reserve).await {
                    Ok(p) => p,
                    Err(_) => {
                        first_err.get_or_insert(PostError::Canceled);
                        break;
                    }
                },
                None => None,
            };
            // the read happens inside the task, so up to READ_AHEAD reads are in flight at once
            // instead of one at a time (network mounts)
            let (me, ctx2, art2, pause2) = (self.clone(), ctx.clone(), art.clone(), pause.cloned());
            let (file2, slots2, path2) = (file.clone(), file_slots.clone(), path.to_path_buf());
            set.spawn(async move {
                let _held = (win, buffer);
                let read = read_at_with_stall_guard(
                    &file2,
                    &slots2,
                    art2.offset as u64,
                    art2.size as usize,
                    &ctx2,
                )
                .await;
                let body = match read {
                    Ok(b) => b,
                    Err(_) if ctx2.is_cancelled() => return Err(PostError::Canceled),
                    Err(e) => {
                        return Err(PostError::Other(format!(
                            "error pre-reading article at offset {} of {}: {e}",
                            art2.offset,
                            path2.display()
                        )));
                    }
                };
                me.upload_one(&ctx2, art2, &body, pause2.as_ref()).await
            });
        }
        while let Some(r) = set.join_next().await {
            collect(Some(r), &mut first_err, &mut completed);
        }
        (completed, first_err)
    }

    /// One article through the worker gates.
    async fn upload_one(
        &self,
        ctx: &CancellationToken,
        art: Article,
        body: &[u8],
        pause: Option<&PauseGate>,
    ) -> Result<Article, PostError> {
        if ctx.is_cancelled() {
            return Err(PostError::Canceled);
        }
        if let Some(p) = pause {
            p.check_pause().await.map_err(|_| PostError::Canceled)?;
        }
        let _shared = tokio::select! {
            () = ctx.cancelled() => return Err(PostError::Canceled),
            p = self.0.workers.clone().acquire_owned() => p.map_err(|_| PostError::Closed)?,
        };
        let _engine = match &self.0.engine {
            Some(e) => Some(
                e.acquire_worker(ctx)
                    .await
                    .map_err(|_| PostError::Canceled)?,
            ),
            None => None,
        };
        if let Some(p) = pause {
            p.check_pause().await.map_err(|_| PostError::Canceled)?;
        }
        let meter = self.0.engine.as_ref().map(|e| e.upload_meter());
        post_yenc(
            ctx,
            self.0.upload.as_ref(),
            self.0.throttle.as_deref(),
            Some(&self.0.stats),
            meter,
            &art,
            body,
        )
        .await?;
        Ok(art)
    }

    /// STATs every article on the verify pool; returns those missing or erroring.
    async fn check_articles(
        &self,
        ctx: &CancellationToken,
        articles: &[Article],
        progress: &Arc<Progress>,
        pause: Option<&PauseGate>,
    ) -> Vec<Article> {
        let limit = Arc::new(Semaphore::new(self.0.verify.max_connections().max(1)));
        let mut set: JoinSet<Option<Article>> = JoinSet::new();
        let mut failed = Vec::new();
        for art in articles {
            // take the permit before spawning so a 100k-article file holds at most `limit` tasks
            let Ok(permit) = limit.clone().acquire_owned().await else {
                break;
            };
            while let Some(r) = set.try_join_next() {
                if let Ok(Some(a)) = r {
                    failed.push(a);
                }
            }
            let (me, ctx2, art2, progress2, pause2) = (
                self.clone(),
                ctx.clone(),
                art.clone(),
                progress.clone(),
                pause.cloned(),
            );
            set.spawn(async move {
                let _p = permit;
                if ctx2.is_cancelled() {
                    return None;
                }
                if let Some(p) = &pause2 {
                    p.check_pause().await.ok()?;
                }
                match me.0.verify.stat(&art2.message_id).await {
                    Ok(true) => {
                        me.0.stats.articles_checked.fetch_add(1, Ordering::Relaxed);
                        progress2.update_progress(art2.size as i64);
                        None
                    }
                    _ => Some(art2), // "article not found" (or a STAT error): needs a re-post
                }
            });
        }
        while let Some(r) = set.join_next().await {
            if let Ok(Some(a)) = r {
                failed.push(a);
            }
        }
        failed.sort_by_key(|a| a.part_number);
        failed
    }
}

fn plan_random_from() -> String {
    article_rs::port::generate_from().unwrap_or_default()
}

fn plan_random_subject() -> String {
    article_rs::port::generate_random_subject().unwrap_or_default()
}

#[cfg(test)]
mod tests;
