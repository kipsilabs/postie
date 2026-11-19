//! Port of the native executor in `internal/par2/par2.go`: block sizing, redundancy, reuse of existing
//! sets, output locations and progress mapping around the creator (`create`).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::create::{InputFile, Options, Par2Error, create_with_names};
use super::existing::{
    Par2Result, check_existing_par2_files_in_path, check_existing_par2_set_in_path,
    par2_set_on_disk, set_input_names, work_dir,
};
use crate::config::{PAR2_GF16_METHODS, Par2Config};
use crate::fileinfo::FileInfo;
use crate::poster::BoxFut;
use crate::progress::{JobProgress, ProgressType};

/// The PAR2 specification's limit on data + recovery blocks (16-bit identifiers: 2^15).
pub const MAX_PAR2_BLOCKS: u64 = 32768;
/// Slice sizes are multiples of this so every SIMD stride divides them.
const SIMD_SAFE_ALIGNMENT: u64 = 128;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExecError {
    #[error("context canceled")]
    Cancelled,
    #[error("{0}")]
    Msg(String),
}

fn msg<T>(s: impl Into<String>) -> Result<T, ExecError> {
    Err(ExecError::Msg(s.into()))
}

/// Creates PAR2 sets; implemented by the native creator, the ParPar binary and test doubles.
pub trait Par2Executor: Send + Sync {
    fn create<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        files: &'a [FileInfo],
    ) -> BoxFut<'a, Result<Par2Result, ExecError>>;
    fn create_in_directory<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        files: &'a [FileInfo],
        output_dir: &'a str,
    ) -> BoxFut<'a, Result<Par2Result, ExecError>>;
    /// Bundles all inputs into one set named `set_name`; each File Description records the path relative
    /// to `folder_dir` so downloaders can recreate the folder tree.
    fn create_set<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        files: &'a [FileInfo],
        output_dir: &'a str,
        set_name: &'a str,
        folder_dir: &'a str,
    ) -> BoxFut<'a, Result<Par2Result, ExecError>>;
}

fn align_down(v: u64, a: u64) -> u64 {
    v / a * a
}

/// Block size for a file: the article size, or the smallest multiple of it that keeps the slice count
/// under 2^15; always a multiple of 4.
pub fn calculate_par_block_size(file_size: u64, article_size: u64) -> u64 {
    if article_size == 0 {
        return 0;
    }
    let block = if file_size / article_size < MAX_PAR2_BLOCKS {
        article_size
    } else {
        let min = file_size / MAX_PAR2_BLOCKS + 1;
        (min / article_size + u64::from(!min.is_multiple_of(article_size))) * article_size
    };
    align_down(block, 4)
}

/// SIMD-safe slice size for a single file; 0 when no safe size exists (file smaller than the alignment).
pub fn compute_file_block_size(file_size: u64, article_size: u64, configured: i64) -> u64 {
    let mut block = match u64::try_from(configured) {
        Ok(c) if c > 0 && c <= file_size => c,
        // a configured size above the file size would give 1-2 slices with a near-empty last one
        _ => calculate_par_block_size(file_size, article_size),
    };
    if file_size > 0 && block > file_size {
        if file_size < SIMD_SAFE_ALIGNMENT {
            return 0;
        }
        block = align_down(file_size, SIMD_SAFE_ALIGNMENT);
    }
    block = align_down(block, SIMD_SAFE_ALIGNMENT);
    if block < SIMD_SAFE_ALIGNMENT {
        0
    } else {
        block
    }
}

/// Redundancy as a percentage of source slices: `"10"`, `"10%"` or the ParPar form `"1n*1.2"`; an
/// unparsable value falls back to 10.
pub fn parse_redundancy_percentage(redundancy: &str) -> f64 {
    let r = redundancy.trim();
    if r.contains('n')
        && let Some((left, right)) = r.split_once('*')
        && let Ok(factor) = right.parse::<f64>()
    {
        let multiplier = left
            .split_once('n')
            .map_or("", |(m, _)| m)
            .parse::<f64>()
            .unwrap_or(1.0);
        return multiplier * factor * 100.0;
    }
    r.trim_end_matches('%').parse::<f64>().unwrap_or_else(|_| {
        eprintln!("par2: could not parse redundancy, defaulting to 10%: redundancy={redundancy}");
        10.0
    })
}

fn recovery_count(slices: u64, pct: f64) -> usize {
    ((slices as f64 * pct / 100.0).ceil() as usize).max(1)
}

fn slice_count(size: u64, block: u64) -> u64 {
    size.div_ceil(block).max(1)
}

/// Go's `filepath.Ext(path) == ".par2"`: the base name's last dot-suffix, case-sensitive.
fn is_par2_ext(path: &str) -> bool {
    base_name(path)
        .rfind('.')
        .is_some_and(|i| base_name(path)[i..] == *".par2")
}

fn base_name(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn parent(p: &str) -> PathBuf {
    Path::new(p)
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default()
}

/// Validates `par2.gf16_method`. The native kernels are chosen by CPU detection, so a pinned method is
/// accepted for config compatibility but does not force a kernel.
fn check_gf16_method(method: &str) -> Result<(), ExecError> {
    if PAR2_GF16_METHODS.contains(&method) || method.is_empty() {
        return Ok(());
    }
    msg(format!(
        "unknown par2 gf16_method {method:?} (accepted: {})",
        PAR2_GF16_METHODS.join(", ")
    ))
}

pub struct NativeExecutor {
    article_size: u64,
    cfg: Par2Config,
    progress: Option<Arc<JobProgress>>,
}

impl NativeExecutor {
    pub fn new(article_size: u64, cfg: Par2Config, progress: Option<Arc<JobProgress>>) -> Self {
        Self {
            article_size,
            cfg,
            progress,
        }
    }

    /// Source directory first (where pre-existing PAR2 files live), then the work dir (a set from an
    /// earlier generation run).
    fn check_existing(&self, source: &FileInfo) -> Option<Vec<String>> {
        let source_dir = parent(&source.path);
        if let Some(found) = check_existing_par2_files_in_path(source, &source_dir) {
            return Some(found);
        }
        let wd = work_dir(&self.cfg);
        if !wd.is_empty() && Path::new(&wd) != source_dir {
            return check_existing_par2_files_in_path(source, Path::new(&wd));
        }
        None
    }

    fn creation_options(&self, slice: u64, num_recovery: usize) -> Options {
        Options {
            slice_size: slice as usize,
            num_recovery,
            num_threads: usize::try_from(self.cfg.num_goroutines).unwrap_or(0),
            memory_limit: self.cfg.memory_limit,
            creator: "Postie".into(),
        }
    }

    /// Runs the creator on a blocking thread with the 0-20 / 20-95 / 95-100 progress mapping.
    async fn run_creation(
        &self,
        cancel: &CancellationToken,
        label: &str,
        output: PathBuf,
        inputs: Vec<InputFile>,
        opts: Options,
    ) -> Result<(), ExecError> {
        let id = Uuid::new_v4();
        let bar = self.progress.as_ref().map(|p| {
            p.add_progress(
                id,
                &format!("PAR2: {label}"),
                ProgressType::Par2Generation,
                100,
            )
        });
        let bar_cb = bar.clone();
        let token = cancel.clone();
        let result = tokio::task::spawn_blocking(move || {
            let cb = move |phase: &str, pct: f64| {
                let Some(pg) = &bar_cb else { return };
                let overall = match phase {
                    "hashing" => pct * 20.0,
                    "encoding" => 20.0 + pct * 75.0,
                    "writing" => 95.0 + pct * 5.0,
                    _ => 0.0,
                };
                let delta = overall as i64 - pg.current();
                if delta > 0 {
                    pg.update_progress(delta);
                }
            };
            create_with_names(&token, &output, &inputs, &opts, Some(&cb))
        })
        .await
        .map_err(|e| ExecError::Msg(format!("par2 creation task failed: {e}")))?;
        if let Some(p) = &self.progress {
            p.finish_progress(id);
        }
        result.map_err(|e| match e {
            Par2Error::Cancelled => ExecError::Cancelled,
            other => ExecError::Msg(other.to_string()),
        })
    }

    async fn create_par2_for_file(
        &self,
        cancel: &CancellationToken,
        file: &FileInfo,
        dir: &Path,
    ) -> Result<Vec<String>, ExecError> {
        let block = compute_file_block_size(file.size, self.article_size, self.cfg.slice_size);
        if block == 0 {
            eprintln!(
                "par2: block size too small for SIMD-safe PAR2 creation, skipping: path={} size={}",
                file.path, file.size
            );
            return Ok(Vec::new());
        }
        check_gf16_method(&self.cfg.gf16_method)?;
        let base = base_name(&file.path);
        let pct = parse_redundancy_percentage(&self.cfg.redundancy);
        let num_recovery = recovery_count(slice_count(file.size, block), pct);
        let output = dir.join(format!("{base}.par2"));
        let inputs = vec![InputFile {
            path: PathBuf::from(&file.path),
            name: String::new(),
        }];
        match self
            .run_creation(
                cancel,
                &base,
                output,
                inputs,
                self.creation_options(block, num_recovery),
            )
            .await
        {
            Ok(()) => {}
            Err(ExecError::Cancelled) => {
                eprintln!("par2: creation cancelled: path={}", file.path);
                return Err(ExecError::Cancelled);
            }
            Err(ExecError::Msg(m)) => {
                return msg(format!("failed to create par2 for {}: {m}", file.path));
            }
        }
        Ok(par2_set_on_disk(dir, &base))
    }

    fn prepare_dir(path: &str, what: &str, fallback: &str) -> PathBuf {
        match std::fs::create_dir_all(path) {
            Ok(()) => PathBuf::from(path),
            Err(e) => {
                eprintln!("par2: failed to create {what}: path={path} error={e}");
                parent(fallback)
            }
        }
    }

    async fn do_create(
        &self,
        cancel: &CancellationToken,
        files: &[FileInfo],
    ) -> Result<Par2Result, ExecError> {
        let mut res = Par2Result::default();
        for file in files {
            if is_par2_ext(&file.path) {
                continue;
            }
            if let Some(existing) = self.check_existing(file) {
                res.reused.extend(existing);
                continue;
            }
            let wd = work_dir(&self.cfg);
            let dir = if wd.is_empty() {
                parent(&file.path)
            } else {
                Self::prepare_dir(&wd, "temp directory", &file.path)
            };
            res.created
                .extend(self.create_par2_for_file(cancel, file, &dir).await?);
        }
        Ok(res)
    }

    async fn do_create_in_directory(
        &self,
        cancel: &CancellationToken,
        files: &[FileInfo],
        output_dir: &str,
    ) -> Result<Par2Result, ExecError> {
        let mut res = Par2Result::default();
        for file in files {
            if is_par2_ext(&file.path) {
                continue;
            }
            let wd = work_dir(&self.cfg);
            let dir = if !output_dir.is_empty() {
                let dir = Self::prepare_dir(output_dir, "output directory", &file.path);
                if dir == Path::new(output_dir) {
                    // pre-existing PAR2 files in the source dir take priority, then the output dir
                    if let Some(e) = check_existing_par2_files_in_path(file, &parent(&file.path))
                        .or_else(|| check_existing_par2_files_in_path(file, &dir))
                    {
                        res.reused.extend(e);
                        continue;
                    }
                }
                dir
            } else if !wd.is_empty() {
                // look for a reusable set before creating the work dir, so a job that reuses
                // source-dir PAR2 files leaves the temp dir untouched
                if let Some(e) = self.check_existing(file) {
                    res.reused.extend(e);
                    continue;
                }
                Self::prepare_dir(&wd, "temp directory", &file.path)
            } else {
                if let Some(e) = self.check_existing(file) {
                    res.reused.extend(e);
                    continue;
                }
                parent(&file.path)
            };
            res.created
                .extend(self.create_par2_for_file(cancel, file, &dir).await?);
        }
        Ok(res)
    }

    /// Slice size for a multi-file set: the configured size when it fits the largest file, else derived
    /// from the total; capped to stay under 2^15 slices and SIMD-aligned. 0 when no safe size exists.
    pub fn compute_set_block_size(&self, total_size: u64, max_file_size: u64) -> u64 {
        let mut block = match u64::try_from(self.cfg.slice_size) {
            Ok(c) if c > 0 && c <= max_file_size => c,
            _ => calculate_par_block_size(total_size, self.article_size),
        };
        if block > 0 && total_size / block >= MAX_PAR2_BLOCKS {
            block = total_size / (MAX_PAR2_BLOCKS - 1) + 1;
        }
        if max_file_size > 0 && block > max_file_size {
            if max_file_size < SIMD_SAFE_ALIGNMENT {
                return 0;
            }
            block = align_down(max_file_size, SIMD_SAFE_ALIGNMENT);
        }
        align_down(block, SIMD_SAFE_ALIGNMENT)
    }

    async fn do_create_set(
        &self,
        cancel: &CancellationToken,
        files: &[FileInfo],
        output_dir: &str,
        set_name: &str,
        folder_dir: &str,
    ) -> Result<Par2Result, ExecError> {
        if files.is_empty() {
            return msg(format!("par2: no input files for set {set_name:?}"));
        }
        if set_name.is_empty() {
            return msg("par2: empty set name");
        }
        check_gf16_method(&self.cfg.gf16_method)?;
        let inputs: Vec<FileInfo> = files
            .iter()
            .filter(|f| !is_par2_ext(&f.path))
            .cloned()
            .collect();
        if inputs.is_empty() {
            return msg(format!(
                "par2: no non-par2 input files for set {set_name:?}"
            ));
        }
        let dir = if !output_dir.is_empty() {
            PathBuf::from(output_dir)
        } else if !work_dir(&self.cfg).is_empty() {
            PathBuf::from(work_dir(&self.cfg))
        } else {
            parent(&inputs[0].path)
        };
        std::fs::create_dir_all(&dir).map_err(|e| {
            ExecError::Msg(format!("par2: create output dir {}: {e}", dir.display()))
        })?;
        let named = set_input_names(&inputs, folder_dir);
        if let Some(existing) = check_existing_par2_set_in_path(set_name, &dir, &named) {
            return Ok(Par2Result {
                reused: existing,
                created: Vec::new(),
            });
        }
        let total: u64 = inputs.iter().map(|f| f.size).sum();
        let max_file = inputs.iter().map(|f| f.size).max().unwrap_or(0);
        let block = self.compute_set_block_size(total, max_file);
        if block < SIMD_SAFE_ALIGNMENT {
            eprintln!(
                "par2: block size too small for SIMD-safe PAR2 set creation, skipping: setName={set_name} totalSize={total} blockSize={block}"
            );
            return Ok(Par2Result::default());
        }
        let total_slices: u64 = inputs.iter().map(|f| slice_count(f.size, block)).sum();
        let pct = parse_redundancy_percentage(&self.cfg.redundancy);
        let num_recovery = recovery_count(total_slices, pct);
        eprintln!(
            "par2: creating PAR2 set: setName={set_name} files={} blockSize={block} inputSlices={total_slices} recoveryBlocks={num_recovery} redundancy={pct}",
            named.len()
        );
        let output = dir.join(format!("{set_name}.par2"));
        match self
            .run_creation(
                cancel,
                set_name,
                output,
                named,
                self.creation_options(block, num_recovery),
            )
            .await
        {
            Ok(()) => {}
            Err(ExecError::Cancelled) => {
                eprintln!("par2: set creation cancelled: setName={set_name}");
                return Err(ExecError::Cancelled);
            }
            Err(ExecError::Msg(m)) => {
                return msg(format!("failed to create par2 set {set_name}: {m}"));
            }
        }
        Ok(Par2Result {
            created: par2_set_on_disk(&dir, set_name),
            reused: Vec::new(),
        })
    }
}

impl Par2Executor for NativeExecutor {
    fn create<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        files: &'a [FileInfo],
    ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
        Box::pin(self.do_create(cancel, files))
    }

    fn create_in_directory<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        files: &'a [FileInfo],
        output_dir: &'a str,
    ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
        Box::pin(self.do_create_in_directory(cancel, files, output_dir))
    }

    fn create_set<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        files: &'a [FileInfo],
        output_dir: &'a str,
        set_name: &'a str,
        folder_dir: &'a str,
    ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
        Box::pin(self.do_create_set(cancel, files, output_dir, set_name, folder_dir))
    }
}

#[cfg(test)]
mod tests;
