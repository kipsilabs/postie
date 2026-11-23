//! PAR2 prefetch: creates the next queued job's PAR2 set while the current job uploads, so a single
//! upload slot does not leave the uplink idle during parity creation.
//!
//! A prefetch runs exactly the executor call the job will later make ([`Par2Request`]). When the job
//! makes that call, [`PrefetchingExecutor`] hands the finished (or still running) result over as if the
//! job had just created it, so ownership, posting and cleanup are unchanged. A result is only used when
//! the inputs' sizes and modification times still match; otherwise its files are deleted before the
//! job creates a fresh set, so a stale set can never be picked up by the reuse check.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::executor::{ExecError, Par2Executor, ProgressRow};
use super::existing::Par2Result;
use crate::fileinfo::FileInfo;
use crate::poster::BoxFut;
use crate::progress::JobProgress;

/// One PAR2 executor call, as a value that can be compared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Par2Request {
    /// `create_in_directory(&[file], output_dir)`.
    File { file: FileInfo, output_dir: String },
    /// `create_set(files, output_dir, set_name, folder_dir)`.
    Set {
        files: Vec<FileInfo>,
        output_dir: String,
        set_name: String,
        folder_dir: String,
    },
}

impl Par2Request {
    pub fn files(&self) -> &[FileInfo] {
        match self {
            Self::File { file, .. } => std::slice::from_ref(file),
            Self::Set { files, .. } => files,
        }
    }

    pub fn output_dir(&self) -> &str {
        match self {
            Self::File { output_dir, .. } | Self::Set { output_dir, .. } => output_dir,
        }
    }

    /// Base name of the set's index file (`<base>.par2`).
    pub fn set_base(&self) -> String {
        match self {
            Self::File { file, .. } => Path::new(&file.path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
            Self::Set { set_name, .. } => set_name.clone(),
        }
    }

    fn label(&self) -> String {
        self.set_base()
    }

    async fn run(
        &self,
        exec: &dyn Par2Executor,
        cancel: &CancellationToken,
    ) -> Result<Par2Result, ExecError> {
        match self {
            Self::File { file, output_dir } => {
                exec.create_in_directory(cancel, std::slice::from_ref(file), output_dir)
                    .await
            }
            Self::Set {
                files,
                output_dir,
                set_name,
                folder_dir,
            } => {
                exec.create_set(cancel, files, output_dir, set_name, folder_dir)
                    .await
            }
        }
    }
}

/// Size and modification time of every input; `None` when one cannot be read.
type Stamp = Vec<(u64, SystemTime)>;

fn stamp(files: &[FileInfo]) -> Option<Stamp> {
    files
        .iter()
        .map(|f| {
            let m = std::fs::metadata(&f.path).ok()?;
            Some((m.len(), m.modified().ok()?))
        })
        .collect()
}

/// Called with created PAR2 paths: `true` keeps them from the work-dir sweeper, `false` releases them.
pub type HoldFn = Arc<dyn Fn(&[String], bool) + Send + Sync>;
type Trigger = Arc<dyn Fn() + Send + Sync>;

struct Entry {
    /// Queue message id of the job the prefetch is for.
    owner: String,
    request: Par2Request,
    stamp: Stamp,
    cancel: CancellationToken,
    handle: JoinHandle<Result<Par2Result, ExecError>>,
}

/// Holds at most one prefetched (or in-flight) PAR2 request.
pub struct Prefetcher {
    slot: Mutex<Option<Entry>>,
    /// Set by [`Prefetcher::close`] while holding `slot`; `start` checks it under the same lock.
    closed: std::sync::atomic::AtomicBool,
    hold: HoldFn,
    trigger: Mutex<Option<Trigger>>,
    hits: AtomicUsize,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Prefetcher {
    pub fn new(hold: HoldFn) -> Self {
        Self {
            slot: Mutex::new(None),
            closed: std::sync::atomic::AtomicBool::new(false),
            hold,
            trigger: Mutex::new(None),
            hits: AtomicUsize::new(0),
        }
    }

    /// Sets what runs after each job's own PAR2 call finishes (the processor queues the next prefetch).
    pub fn set_trigger(&self, f: Trigger) {
        *lock(&self.trigger) = Some(f);
    }

    fn fire_trigger(&self) {
        let f = lock(&self.trigger).clone();
        if let Some(f) = f {
            f();
        }
    }

    /// How many job PAR2 calls were served from a prefetch.
    pub fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }

    /// Queue message id of the held prefetch, if any.
    pub fn owner(&self) -> Option<String> {
        lock(&self.slot).as_ref().map(|e| e.owner.clone())
    }

    /// Starts creating `request`'s set for queue item `owner` on `exec`. Returns the prefetch it
    /// replaced (to be passed to [`Prefetcher::discard`]); a prefetch of the same request with unchanged
    /// inputs is kept instead.
    #[must_use = "a replaced prefetch must be discarded"]
    pub fn start(
        &self,
        owner: &str,
        request: Par2Request,
        exec: Arc<dyn Par2Executor>,
    ) -> Option<Discarded> {
        let stamp = stamp(request.files())?;
        let mut slot = lock(&self.slot);
        if self.closed.load(Ordering::SeqCst) {
            return None;
        }
        if slot
            .as_ref()
            .is_some_and(|e| e.request == request && e.stamp == stamp)
        {
            return None;
        }
        let cancel = CancellationToken::new();
        let (req, token, hold) = (request.clone(), cancel.clone(), self.hold.clone());
        let handle = tokio::spawn(async move {
            let res = req.run(&*exec, &token).await;
            if let Ok(r) = &res {
                hold(&r.created, true);
            }
            res
        });
        let old = slot.replace(Entry {
            owner: owner.to_string(),
            request,
            stamp,
            cancel,
            handle,
        });
        old.map(|e| Discarded(e, self.hold.clone()))
    }

    /// Removes the held prefetch unless it belongs to queue item `owner`.
    #[must_use = "a removed prefetch must be discarded"]
    pub fn take_unless_owner(&self, owner: &str) -> Option<Discarded> {
        let mut slot = lock(&self.slot);
        if slot.as_ref().is_some_and(|e| e.owner != owner) {
            return slot.take().map(|e| Discarded(e, self.hold.clone()));
        }
        None
    }

    /// Stops all prefetching for good and removes the held prefetch. A planner that is already past
    /// its own checks cannot register anything afterwards: the flag is set under the slot lock that
    /// `start` takes.
    #[must_use = "a removed prefetch must be discarded"]
    pub fn close(&self) -> Option<Discarded> {
        let mut slot = lock(&self.slot);
        self.closed.store(true, Ordering::SeqCst);
        slot.take().map(|e| Discarded(e, self.hold.clone()))
    }

    /// Removes a held prefetch that is not `request` but would write the same set file.
    fn take_colliding(&self, request: &Par2Request) -> Option<Discarded> {
        let mut slot = lock(&self.slot);
        if slot.as_ref().is_some_and(|e| {
            e.request != *request
                && e.request.output_dir() == request.output_dir()
                && e.request.set_base() == request.set_base()
        }) {
            return slot.take().map(|e| Discarded(e, self.hold.clone()));
        }
        None
    }

    fn take_matching(&self, request: &Par2Request) -> Option<Entry> {
        let mut slot = lock(&self.slot);
        if slot.as_ref().is_some_and(|e| e.request == *request) {
            return slot.take();
        }
        None
    }
}

/// A prefetch removed from the store; [`Discarded::discard`] cancels it and deletes what it created.
pub struct Discarded(Entry, HoldFn);

impl Discarded {
    pub async fn discard(self) {
        let Self(entry, hold) = self;
        entry.cancel.cancel();
        if let Ok(Ok(res)) = entry.handle.await {
            hold(&res.created, false);
            remove_files(&res.created);
        }
    }
}

fn remove_files(paths: &[String]) {
    for p in paths {
        if let Err(e) = std::fs::remove_file(p)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            crate::logx::warn(
                &format!("Could not remove unused prefetched PAR2 file: path={p} error={e}"),
                &[("component", "par2".into())],
            );
        }
    }
}

/// Serves a job's PAR2 calls from the [`Prefetcher`] when it holds the same request, else from `inner`.
pub struct PrefetchingExecutor {
    inner: Arc<dyn Par2Executor>,
    store: Arc<Prefetcher>,
    progress: Option<Arc<JobProgress>>,
}

impl PrefetchingExecutor {
    pub fn wrap(
        inner: Arc<dyn Par2Executor>,
        store: Arc<Prefetcher>,
        progress: Option<Arc<JobProgress>>,
    ) -> Arc<dyn Par2Executor> {
        Arc::new(Self {
            inner,
            store,
            progress,
        })
    }

    async fn serve(
        &self,
        cancel: &CancellationToken,
        request: Par2Request,
    ) -> Result<Par2Result, ExecError> {
        let res = match self.prefetched(cancel, &request).await {
            Some(r) => r,
            None => {
                // a stale prefetch of the same set file must be gone before the job writes it, or its
                // later discard would delete the job's own set
                if let Some(stale) = self.store.take_colliding(&request) {
                    stale.discard().await;
                }
                request.run(&*self.inner, cancel).await
            }
        };
        if !matches!(res, Err(ExecError::Cancelled)) {
            self.store.fire_trigger();
        }
        res
    }

    /// The prefetched result for `request`, or `None` when there is none or it is unusable (its files
    /// are then already deleted). A job cancelled while waiting gets `Cancelled`.
    async fn prefetched(
        &self,
        cancel: &CancellationToken,
        request: &Par2Request,
    ) -> Option<Result<Par2Result, ExecError>> {
        let entry = self.store.take_matching(request)?;
        let hold = self.store.hold.clone();
        let fresh = stamp(request.files()).is_some_and(|s| s == entry.stamp);
        if !fresh {
            Discarded(entry, hold).discard().await;
            return None;
        }
        let row = (!entry.handle.is_finished()).then(|| {
            ProgressRow::add(
                self.progress.as_ref(),
                &format!("PAR2: {} (prefetched)", request.label()),
            )
        });
        let Entry {
            cancel: token,
            mut handle,
            ..
        } = entry;
        let joined = tokio::select! {
            r = &mut handle => r,
            () = cancel.cancelled() => {
                token.cancel();
                if let Ok(Ok(res)) = handle.await {
                    hold(&res.created, false);
                    remove_files(&res.created);
                }
                return Some(Err(ExecError::Cancelled));
            }
        };
        drop(row);
        match joined {
            Ok(Ok(res)) if res.created.iter().all(|p| Path::new(p).exists()) => {
                // the job's own tracking reserves the files again right after this returns
                hold(&res.created, false);
                self.store.hits.fetch_add(1, Ordering::SeqCst);
                crate::logx::info(
                    &format!(
                        "Using prefetched PAR2 set: set={} files={}",
                        request.label(),
                        res.all().len()
                    ),
                    &[("component", "par2".into())],
                );
                Some(Ok(res))
            }
            Ok(Ok(res)) => {
                hold(&res.created, false);
                remove_files(&res.created);
                None
            }
            // a failed or cancelled prefetch is simply redone by the job
            Ok(Err(_)) | Err(_) => None,
        }
    }
}

impl Par2Executor for PrefetchingExecutor {
    fn create<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        files: &'a [FileInfo],
    ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
        self.inner.create(cancel, files)
    }

    fn create_in_directory<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        files: &'a [FileInfo],
        output_dir: &'a str,
    ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
        match files {
            [file] => Box::pin(self.serve(
                cancel,
                Par2Request::File {
                    file: file.clone(),
                    output_dir: output_dir.to_string(),
                },
            )),
            _ => self.inner.create_in_directory(cancel, files, output_dir),
        }
    }

    fn create_set<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        files: &'a [FileInfo],
        output_dir: &'a str,
        set_name: &'a str,
        folder_dir: &'a str,
    ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
        Box::pin(self.serve(
            cancel,
            Par2Request::Set {
                files: files.to_vec(),
                output_dir: output_dir.to_string(),
                set_name: set_name.to_string(),
                folder_dir: folder_dir.to_string(),
            },
        ))
    }
}

#[cfg(test)]
mod tests;
