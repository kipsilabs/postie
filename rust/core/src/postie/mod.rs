//! Port of `pkg/postie`: the orchestrator that turns files into uploaded articles plus an NZB —
//! duplicate lookup, PAR2 creation (before or alongside the upload), posting, NZB generation, PAR2
//! cleanup and the durable-transfer hand-off.

pub mod paths;
pub mod script;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use chrono::Utc;
use tokio_util::sync::CancellationToken;

use crate::config::{
    ConfigData, DuplicateLookupConfig, NzbCompressionConfig, Par2Config, PostCheck,
    PostUploadScriptConfig, PostingConfig,
};
use crate::duplicatelookup;
use crate::engine::Engine;
use crate::fileinfo::FileInfo;
use crate::nzb::{self, Generator};
use crate::par2::binary::new_executor;
use crate::par2::executor::{ExecError, Par2Executor};
use crate::par2::existing::{Par2Result, work_dir};
use crate::par2::is_par2_file;
use crate::par2::scheduler::{ScheduledExecutor, Scheduler};
use crate::pausable::PauseGate;
use crate::poster::{ArticleClient, DeferredCheckError, ManifestSink, PostError, Poster};
use crate::progress::JobProgress;
use crate::transferwriter::Recorder;
use paths::{additional_par2_paths, derive_folder_name, relative_path_from};
use script::{ScriptQueue, run_post_upload_script};

pub mod runtime;
pub use runtime::Runtime;

#[derive(Debug, thiserror::Error)]
pub enum PostieError {
    #[error("context canceled")]
    Canceled,
    #[error("{0}")]
    Post(PostError),
    #[error("{0}")]
    Msg(String),
}

fn msg<T>(s: impl Into<String>) -> Result<T, PostieError> {
    Err(PostieError::Msg(s.into()))
}

/// A finished upload: the NZB path and, when some articles still need deferred verification, the
/// non-fatal [`DeferredCheckError`] (the NZB was generated regardless).
#[derive(Debug, Clone, PartialEq)]
pub struct PostOutcome {
    pub nzb_path: String,
    pub deferred: Option<DeferredCheckError>,
}

fn is_canceled(e: &PostError) -> bool {
    matches!(e, PostError::Canceled)
}

/// Tolerates files held briefly open (Windows): retries, then a final best-effort delete.
async fn safe_remove_file(cancel: &CancellationToken, path: &str) {
    for i in 0..3u64 {
        if std::fs::remove_file(path).is_ok() {
            return;
        }
        if i < 2 {
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(std::time::Duration::from_millis(100 * (i + 1))) => {}
            }
        }
    }
    let _ = std::fs::remove_file(path);
}

/// PAR2 problems never fail the upload, so they must at least be visible: stderr is invisible in the
/// desktop app, so they also go to postie.log.
fn par2_failure(message: &str) {
    eprintln!("{message}");
    crate::logx::warn("PAR2 skipped for this upload", &[("error", message.into())]);
}

fn has_par2_files(files: &[FileInfo]) -> bool {
    files.iter().any(|f| is_par2_file(&f.path))
}

/// Removes PAR2 files from the NZB after their upload failed part-way: an NZB pointing at an incomplete
/// set is worse than one without PAR2.
fn drop_par2_from_nzb(nzb: &Generator, par2_paths: &[String]) {
    for p in par2_paths {
        nzb.remove_file(
            &Path::new(p)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
    }
}

fn paths_of(v: &[String]) -> Vec<PathBuf> {
    v.iter().map(PathBuf::from).collect()
}

pub struct Postie {
    duplicate_cfg: DuplicateLookupConfig,
    par2_cfg: Par2Config,
    posting_cfg: PostingConfig,
    compression_cfg: NzbCompressionConfig,
    script_cfg: PostUploadScriptConfig,
    maintain_original_extension: bool,
    check_cfg: PostCheck,
    par2: Arc<dyn Par2Executor>,
    poster: Poster,
    progress: Arc<JobProgress>,
    queue: Option<Arc<dyn ScriptQueue>>,
    /// Per-job durable manifest recorder; `None` in standalone mode.
    recorder: Option<Arc<Recorder>>,
    runtime: Option<Arc<Runtime>>,
    reserved_par2: Mutex<Vec<String>>,
    reused_par2: Mutex<Vec<String>>,
    reused_sources: Mutex<HashSet<String>>,
    source_paths: Mutex<Vec<String>>,
    delete_original: AtomicBool,
    pause: Option<Arc<PauseGate>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Postie {
    /// Builds a Postie borrowing shared resources from `rt` (PAR2 scheduler, upload engine, durable
    /// store). Without a runtime a private PAR2 scheduler honouring `par2.max_concurrent_jobs` is
    /// created and manifest recording is off. `transfer_id` binds this job's durable manifests.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cfg: &ConfigData,
        upload: Arc<dyn ArticleClient>,
        verify: Arc<dyn ArticleClient>,
        progress: Arc<JobProgress>,
        queue: Option<Arc<dyn ScriptQueue>>,
        rt: Option<Arc<Runtime>>,
        transfer_id: &str,
        pause: Option<Arc<PauseGate>>,
    ) -> Self {
        let par2_cfg = cfg.par2.clone();
        let posting_cfg = cfg.posting.clone();
        let executor = new_executor(
            posting_cfg.article_size_in_bytes,
            &par2_cfg,
            Some(progress.clone()),
        );
        let scheduler = rt.as_ref().map_or_else(
            || Arc::new(Scheduler::new(par2_cfg.max_concurrent_jobs.max(1))),
            |r| r.par2_scheduler(),
        );
        let par2 = ScheduledExecutor::wrap(executor, Some(scheduler));
        let recorder = rt
            .as_ref()
            .and_then(|r| r.new_manifest_recorder(transfer_id));
        let sink: Option<Arc<dyn ManifestSink>> =
            recorder.clone().map(|r| r as Arc<dyn ManifestSink>);
        let engine: Option<Arc<Engine>> = rt.as_ref().and_then(|r| r.upload_engine());
        let poster = Poster::new(
            posting_cfg.clone(),
            cfg.post_check.clone(),
            upload,
            verify,
            progress.clone(),
            engine,
            sink,
        );
        Self {
            duplicate_cfg: cfg.duplicate_lookup.clone(),
            par2_cfg,
            posting_cfg,
            compression_cfg: cfg.nzb_compression.clone(),
            script_cfg: cfg.post_upload_script.clone(),
            maintain_original_extension: cfg.maintain_original_extension(),
            check_cfg: cfg.post_check.clone(),
            par2,
            poster,
            progress,
            queue,
            recorder,
            runtime: rt,
            reserved_par2: Mutex::new(Vec::new()),
            reused_par2: Mutex::new(Vec::new()),
            reused_sources: Mutex::new(HashSet::new()),
            source_paths: Mutex::new(Vec::new()),
            delete_original: AtomicBool::new(false),
            pause,
        }
    }

    /// Whether the job's originals are deleted once the durable verification confirms the upload.
    /// Must be set before `post`; no effect in standalone mode.
    pub fn set_delete_original(&self, v: bool) {
        self.delete_original.store(v, Ordering::SeqCst);
    }

    pub fn poster(&self) -> &Poster {
        &self.poster
    }

    pub fn job_progress(&self) -> &Arc<JobProgress> {
        &self.progress
    }

    fn pause(&self) -> Option<&PauseGate> {
        self.pause.as_deref()
    }

    /// Generated PAR2 files are deleted right after a successful post only in standalone mode with
    /// `maintain_par2_files` off: in durable mode the background verification may still re-post PAR2
    /// articles from them, and the transfer cleaner removes them once verified.
    fn remove_par2_after_post(&self) -> bool {
        self.recorder.is_none() && self.par2_cfg.maintain_par2_files != Some(true)
    }

    /// Registers the PAR2 files about to be posted: the recorder marks created ones as generated, and all
    /// of them (reused included) are reserved so the work-dir sweeper cannot remove a set mid-upload.
    fn track_generated(&self, res: &Par2Result) {
        let all = res.all();
        if all.is_empty() {
            return;
        }
        if let Some(r) = &self.recorder
            && !res.created.is_empty()
        {
            r.mark_generated(res.created.clone());
        }
        if let Some(rt) = &self.runtime {
            rt.reserve_par2(&all);
        }
        lock(&self.reserved_par2).extend(all);
        lock(&self.reused_par2).extend(res.reused.iter().cloned());
    }

    /// The pre-existing PAR2 files posted by the last `post`.
    pub fn reused_par2_files(&self) -> Vec<String> {
        lock(&self.reused_par2).clone()
    }

    /// Whether this source reused an existing NZB in the last `post`.
    pub fn reused_source(&self, path: &str) -> bool {
        lock(&self.reused_sources).contains(path)
    }

    /// Marks the transfer's files uploaded so the durable verification service picks them up, scheduling
    /// the first check after the propagation delay.
    fn complete_transfer_upload(&self) {
        let Some(r) = &self.recorder else { return };
        let delay = chrono::Duration::nanoseconds(self.check_cfg.retry_delay.nanos().max(0));
        let now = Utc::now();
        if let Err(e) = r.complete_upload(
            now,
            now + delay,
            self.delete_original.load(Ordering::SeqCst),
        ) {
            eprintln!("postie: failed to mark transfer uploaded for verification: {e}");
        }
    }

    /// Closes the poster and releases this job's PAR2 reservations.
    pub fn close(&self) {
        self.poster.close();
        if let Some(rt) = &self.runtime {
            let paths = std::mem::take(&mut *lock(&self.reserved_par2));
            rt.release_par2(&paths);
        }
    }

    /// Removes the PAR2 files for `source` (when a job fails permanently and would otherwise orphan them).
    pub async fn cleanup_par2_files(&self, cancel: &CancellationToken, source: &FileInfo) {
        let wd = work_dir(&self.par2_cfg);
        let dir = if wd.is_empty() {
            Path::new(&source.path)
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default()
        } else {
            PathBuf::from(wd)
        };
        let base = Path::new(&source.path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let main = dir.join(format!("{base}.par2"));
        if main.exists() {
            safe_remove_file(cancel, &main.to_string_lossy()).await;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return eprintln!(
                "postie: failed to read directory for PAR2 cleanup: {}",
                dir.display()
            );
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            if name.starts_with(&base) && name.contains(".vol") && name.ends_with(".par2") {
                safe_remove_file(cancel, &dir.join(&name).to_string_lossy()).await;
            }
        }
        eprintln!("postie: PAR2 cleanup completed: sourceFile={}", source.path);
    }

    fn new_generator(&self) -> Arc<Generator> {
        Arc::new(Generator::new(
            self.posting_cfg.article_size_in_bytes,
            self.compression_cfg.clone(),
            self.maintain_original_extension,
        ))
    }

    fn wait_for_par2(&self) -> bool {
        self.posting_cfg.wait_for_par2.unwrap_or(false)
    }

    fn par2_enabled(&self) -> bool {
        self.par2_cfg.enabled.unwrap_or(true)
    }

    /// Posts `files`: one NZB per file, or a single NZB for the folder when `force_folder_mode`.
    pub async fn post(
        &self,
        cancel: &CancellationToken,
        files: &[FileInfo],
        root_dir: &str,
        output_dir: &str,
        force_folder_mode: bool,
    ) -> Result<PostOutcome, PostieError> {
        if files.is_empty() {
            return msg("no files to post");
        }
        lock(&self.reused_sources).clear();
        *lock(&self.source_paths) = files.iter().map(|f| f.path.clone()).collect();
        let result = if force_folder_mode {
            self.post_folder(cancel, files, root_dir, output_dir).await
        } else {
            self.post_each(cancel, files, root_dir, output_dir).await
        };
        // On success with something actually uploaded, hand the transfer to the verification service.
        // (Go skips this when a deferred error is returned; durable mode never produces one.)
        if let Ok(o) = &result
            && o.deferred.is_none()
            && lock(&self.reused_sources).len() < files.len()
        {
            if !lock(&self.reused_sources).is_empty() {
                self.delete_original.store(false, Ordering::SeqCst);
            }
            self.complete_transfer_upload();
        }
        result
    }

    async fn post_each(
        &self,
        cancel: &CancellationToken,
        files: &[FileInfo],
        root_dir: &str,
        output_dir: &str,
    ) -> Result<PostOutcome, PostieError> {
        let start = Instant::now();
        let mut last_nzb = String::new();
        let mut last_deferred: Option<DeferredCheckError> = None;
        for f in files {
            eprintln!("postie: posting file: {}", f.path);
            if self.duplicate_cfg.enabled {
                let timeout = std::time::Duration::from_nanos(
                    u64::try_from(self.duplicate_cfg.timeout.nanos()).unwrap_or(0),
                );
                let data = duplicatelookup::lookup(
                    cancel,
                    &self.duplicate_cfg.url,
                    &self.duplicate_cfg.api_key,
                    timeout,
                    Path::new(&f.path),
                )
                .await
                .map_err(|e| PostieError::Msg(format!("duplicate lookup: {e}")))?;
                if let Some(data) = data {
                    let target = Path::new(output_dir)
                        .join(relative_path_from(root_dir, &f.path))
                        .join(Path::new(&f.path).file_name().unwrap_or_default());
                    let saved = nzb::save_existing(
                        cancel,
                        &data,
                        &target,
                        &self.compression_cfg,
                        self.maintain_original_extension,
                    )
                    .map_err(|e| PostieError::Msg(e.to_string()))?;
                    lock(&self.reused_sources).insert(f.path.clone());
                    last_nzb = saved.to_string_lossy().into_owned();
                    continue;
                }
            }
            let out = if self.wait_for_par2() {
                self.post_serial(cancel, f, root_dir, output_dir).await
            } else {
                self.post_parallel(cancel, f, root_dir, output_dir).await
            }?;
            if let Some(de) = out.deferred {
                last_deferred = Some(de);
            }
            last_nzb = out.nzb_path;
        }
        if lock(&self.reused_sources).len() == files.len() {
            return Ok(PostOutcome {
                nzb_path: last_nzb,
                deferred: None,
            });
        }
        let s = self.poster.stats();
        eprintln!(
            "postie: upload completed in {:?}; articles posted={} checked={} bytes={} errors={}",
            start.elapsed(),
            s.articles_posted,
            s.articles_checked,
            s.bytes_posted,
            s.article_errors
        );
        Ok(PostOutcome {
            nzb_path: last_nzb,
            deferred: last_deferred,
        })
    }

    /// Where to write PAR2 files: straight into the output dir when `maintain_par2_files` is on.
    fn par2_output_dir(&self, root_dir: &str, output_dir: &str, f: &FileInfo) -> String {
        if self.par2_cfg.maintain_par2_files == Some(true) {
            Path::new(output_dir)
                .join(relative_path_from(root_dir, &f.path))
                .to_string_lossy()
                .into_owned()
        } else {
            String::new()
        }
    }

    fn nzb_target(&self, root_dir: &str, output_dir: &str, f: &FileInfo) -> PathBuf {
        Path::new(output_dir)
            .join(relative_path_from(root_dir, &f.path))
            .join(Path::new(&f.path).file_name().unwrap_or_default())
    }

    async fn cleanup_created(&self, cancel: &CancellationToken, res: &Par2Result) {
        if self.remove_par2_after_post() {
            for p in &res.created {
                safe_remove_file(cancel, p).await;
            }
        }
    }

    /// PAR2 first, then everything in one post call (`wait_for_par2`).
    async fn post_serial(
        &self,
        cancel: &CancellationToken,
        f: &FileInfo,
        root_dir: &str,
        output_dir: &str,
    ) -> Result<PostOutcome, PostieError> {
        let nzb = self.new_generator();
        let mut par2_res = Par2Result::default();
        let mut files_path = vec![f.path.clone()];
        if self.par2_enabled() {
            let dir = self.par2_output_dir(root_dir, output_dir, f);
            par2_res = match self
                .par2
                .create_in_directory(cancel, std::slice::from_ref(f), &dir)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    if e != ExecError::Cancelled {
                        par2_failure(&format!(
                            "postie: error during par2 creation. Upload will continue without par2: {e}"
                        ));
                    }
                    if e == ExecError::Cancelled {
                        return Err(PostieError::Canceled);
                    }
                    Par2Result::default()
                }
            };
            self.track_generated(&par2_res);
            files_path.extend(additional_par2_paths(
                &par2_res.all(),
                &lock(&self.source_paths),
            ));
        }
        let mut deferred = None;
        if let Err(e) = self
            .poster
            .post(cancel, &paths_of(&files_path), &nzb, self.pause())
            .await
        {
            match e {
                PostError::Deferred(de) => {
                    eprintln!(
                        "postie: some articles deferred for later verification: file={} deferred={}",
                        f.path,
                        de.failed_articles.len()
                    );
                    deferred = Some(de);
                }
                other => {
                    if !is_canceled(&other) {
                        eprintln!("postie: error during upload: {files_path:?}: {other}");
                    }
                    return Err(if is_canceled(&other) {
                        PostieError::Canceled
                    } else {
                        PostieError::Post(other)
                    });
                }
            }
        }
        let final_path = nzb
            .generate(&self.nzb_target(root_dir, output_dir, f))
            .map_err(|e| PostieError::Msg(format!("error generating NZB file: {e}")))?;
        self.cleanup_created(cancel, &par2_res).await;
        Ok(PostOutcome {
            nzb_path: final_path.to_string_lossy().into_owned(),
            deferred,
        })
    }

    /// PAR2 creation and the upload run side by side; PAR2 failures are tolerated.
    async fn post_parallel(
        &self,
        cancel: &CancellationToken,
        f: &FileInfo,
        root_dir: &str,
        output_dir: &str,
    ) -> Result<PostOutcome, PostieError> {
        let nzb = self.new_generator();
        // A failed main upload cancels the PAR2 work instead of letting it run to completion.
        let par2_cancel = &cancel.child_token();
        let par2_branch = async {
            let dir = self.par2_output_dir(root_dir, output_dir, f);
            let res = match self
                .par2
                .create_in_directory(par2_cancel, std::slice::from_ref(f), &dir)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    if e != ExecError::Cancelled {
                        par2_failure(&format!(
                            "postie: error during par2 creation. Upload will continue without par2: {e}"
                        ));
                    }
                    return Par2Result::default();
                }
            };
            self.track_generated(&res);
            let par2_paths = additional_par2_paths(&res.all(), &lock(&self.source_paths));
            if par2_paths.is_empty() {
                return res;
            }
            if let Err(e) = self
                .poster
                .post(par2_cancel, &paths_of(&par2_paths), &nzb, self.pause())
                .await
            {
                if !is_canceled(&e) {
                    par2_failure(&format!(
                        "postie: error during upload of par2 files {par2_paths:?}: {e}. Upload will continue without par2."
                    ));
                }
                drop_par2_from_nzb(&nzb, &par2_paths);
            }
            res
        };
        let main_branch = async {
            match self
                .poster
                .post(cancel, &[PathBuf::from(&f.path)], &nzb, self.pause())
                .await
            {
                Ok(()) => Ok(None),
                Err(PostError::Deferred(de)) => Ok(Some(de)),
                Err(e) => {
                    if !is_canceled(&e) {
                        eprintln!("postie: error during upload: {}: {e}", f.path);
                    }
                    par2_cancel.cancel();
                    Err(if is_canceled(&e) {
                        PostieError::Canceled
                    } else {
                        PostieError::Post(e)
                    })
                }
            }
        };
        let (par2_res, main) = tokio::join!(par2_branch, main_branch);
        let deferred = main?;
        if cancel.is_cancelled() {
            return Err(PostieError::Canceled);
        }
        let final_path = nzb
            .generate(&self.nzb_target(root_dir, output_dir, f))
            .map_err(|e| PostieError::Msg(format!("error generating NZB file: {e}")))?;
        self.cleanup_created(cancel, &par2_res).await;
        Ok(PostOutcome {
            nzb_path: final_path.to_string_lossy().into_owned(),
            deferred,
        })
    }

    /// Posts a whole folder as a single NZB; its PAR2 set (one set, FileDesc paths relative to the folder
    /// root) is created first or alongside the upload.
    async fn post_folder(
        &self,
        cancel: &CancellationToken,
        files: &[FileInfo],
        root_dir: &str,
        output_dir: &str,
    ) -> Result<PostOutcome, PostieError> {
        let start = Instant::now();
        let folder = derive_folder_name(root_dir, files);
        let folder_out = Path::new(output_dir).join(&folder);
        eprintln!(
            "postie: posting folder as single NZB: folder={folder} files={}",
            files.len()
        );
        let nzb = self.new_generator();
        let mut all_paths: Vec<String> = files.iter().map(|f| f.path.clone()).collect();
        let relative: HashMap<String, String> = files
            .iter()
            .filter(|f| !f.relative_path.is_empty())
            .map(|f| (f.path.clone(), f.relative_path.clone()))
            .collect();
        let folder_dir = Path::new(root_dir)
            .join(&folder)
            .to_string_lossy()
            .into_owned();
        let par2_out = if self.par2_cfg.maintain_par2_files == Some(true) {
            folder_out.to_string_lossy().into_owned()
        } else {
            String::new()
        };
        let skip_par2 = !self.par2_enabled()
            || (self.par2_cfg.skip_if_par2_exists == Some(true) && has_par2_files(files));
        if self.par2_enabled() && skip_par2 {
            eprintln!(
                "postie: skipping PAR2 generation: existing PAR2 files detected in source folder: folder={folder}"
            );
        }
        let nzb_path = folder_out.join(format!("{folder}.nzb"));
        let mut par2_res = Par2Result::default();
        let deferred;
        if self.wait_for_par2() {
            if !skip_par2 {
                match self
                    .par2
                    .create_set(cancel, files, &par2_out, &folder, &folder_dir)
                    .await
                {
                    Ok(r) => {
                        self.track_generated(&r);
                        all_paths.extend(additional_par2_paths(&r.all(), &all_paths));
                        par2_res = r;
                    }
                    Err(e) => {
                        if e != ExecError::Cancelled {
                            par2_failure(&format!(
                                "postie: error during par2 creation. Upload will continue without par2: {e}"
                            ));
                        }
                    }
                }
            }
            deferred = match self
                .poster
                .post_with_relative_paths(
                    cancel,
                    &paths_of(&all_paths),
                    &nzb,
                    &relative,
                    self.pause(),
                )
                .await
            {
                Ok(()) => None,
                Err(PostError::Deferred(de)) => {
                    eprintln!(
                        "postie: some articles deferred for later verification: folder={folder} deferred={}",
                        de.failed_articles.len()
                    );
                    Some(de)
                }
                Err(e) => {
                    if !is_canceled(&e) {
                        eprintln!("postie: error during folder upload: folder={folder}: {e}");
                    }
                    return Err(if is_canceled(&e) {
                        PostieError::Canceled
                    } else {
                        PostieError::Post(e)
                    });
                }
            };
            let final_path = nzb.generate(&nzb_path).map_err(|e| {
                PostieError::Msg(format!("error generating NZB file for folder: {e}"))
            })?;
            self.cleanup_created(cancel, &par2_res).await;
            return Ok(PostOutcome {
                nzb_path: final_path.to_string_lossy().into_owned(),
                deferred,
            });
        }

        // A failed main upload cancels the PAR2 work instead of letting it run to completion.
        let par2_cancel = &cancel.child_token();
        let par2_branch = async {
            if skip_par2 {
                return Par2Result::default();
            }
            let res = match self
                .par2
                .create_set(par2_cancel, files, &par2_out, &folder, &folder_dir)
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    if e != ExecError::Cancelled {
                        par2_failure(&format!(
                            "postie: error during par2 creation. Upload will continue without par2: {e}"
                        ));
                    }
                    return Par2Result::default();
                }
            };
            self.track_generated(&res);
            let par2_paths = additional_par2_paths(&res.all(), &all_paths);
            if par2_paths.is_empty() {
                return res;
            }
            if let Err(e) = self
                .poster
                .post_with_relative_paths(
                    par2_cancel,
                    &paths_of(&par2_paths),
                    &nzb,
                    &HashMap::new(),
                    self.pause(),
                )
                .await
            {
                if !is_canceled(&e) {
                    par2_failure(&format!(
                        "postie: error during upload of par2 files. Upload will continue without par2: {e}"
                    ));
                }
                drop_par2_from_nzb(&nzb, &par2_paths);
            }
            res
        };
        let main_branch = async {
            match self
                .poster
                .post_with_relative_paths(
                    cancel,
                    &paths_of(&all_paths),
                    &nzb,
                    &relative,
                    self.pause(),
                )
                .await
            {
                Ok(()) => Ok(None),
                Err(PostError::Deferred(de)) => Ok(Some(de)),
                Err(e) => {
                    if !is_canceled(&e) {
                        eprintln!("postie: error during folder upload: folder={folder}: {e}");
                    }
                    par2_cancel.cancel();
                    Err(if is_canceled(&e) {
                        PostieError::Canceled
                    } else {
                        PostieError::Post(e)
                    })
                }
            }
        };
        let (res, main) = tokio::join!(par2_branch, main_branch);
        let deferred = main?;
        if cancel.is_cancelled() {
            return Err(PostieError::Canceled);
        }
        let final_path = nzb
            .generate(&nzb_path)
            .map_err(|e| PostieError::Msg(format!("error generating NZB file for folder: {e}")))?;
        self.cleanup_created(cancel, &res).await;
        let s = self.poster.stats();
        eprintln!(
            "postie: folder upload completed: folder={folder} elapsed={:?} posted={} checked={} bytes={} errors={}",
            start.elapsed(),
            s.articles_posted,
            s.articles_checked,
            s.bytes_posted,
            s.article_errors
        );
        Ok(PostOutcome {
            nzb_path: final_path.to_string_lossy().into_owned(),
            deferred,
        })
    }

    /// Runs the post-upload script for a completed item (after it was marked completed in the database).
    pub async fn execute_post_upload_script(
        &self,
        cancel: &CancellationToken,
        nzb_path: &str,
        source_path: &str,
        item_id: &str,
    ) -> Result<(), String> {
        run_post_upload_script(
            cancel,
            &self.script_cfg,
            self.queue.as_deref(),
            nzb_path,
            source_path,
            item_id,
        )
        .await
    }
}

#[cfg(test)]
mod tests;
