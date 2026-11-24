//! PAR2 creation (ADR-021): a pipelined scan plus GF(2^16) Reed-Solomon encode.
//!
//! One reader thread streams each input file (in file-ID order, the order PAR2 numbers source slices)
//! in batches of zero-padded slices. Per batch, a worker pool hashes slices (MD5 + CRC32 for the IFSC
//! packets) and encodes: every worker owns a column range of all recovery buffers, so there is no
//! locking and no `unsafe`. Each worker walks its range in `TILE_BYTES` tiles so a tile's sources
//! stay in cache while every recovery block is updated. The whole-file MD5, which must be sequential,
//! runs on one thread beside the pool (which leaves it a core by default), so the single-stream MD5
//! rate is the only floor. When the recovery buffers would
//! exceed the memory limit the encode is split into passes that re-read the inputs.

use std::fs::File;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::sync_channel;

use md5::{Digest, Md5};
use reedsolomon_rs::gf;
use reedsolomon_rs::gf_simd::{FactorSrc, input_batch_width, mul_acc_input_batch, mul_acc_region};
use tokio_util::sync::CancellationToken;

use super::packets::{self, IfscEntry, Md5Sum};

const HASH16K_LEN: usize = 16 * 1024;
/// Bytes of source data per batch (a batch always holds at least one whole slice).
const BATCH_BYTES: usize = 64 * 1024 * 1024;
const MAX_BATCH_SLICES: usize = 4096;
const DEFAULT_MEMORY_LIMIT: i64 = 4 * 1024 * 1024 * 1024;
/// Column ranges are multiples of a cache line so workers never share one.
const COLUMN_ALIGN: usize = 64;
/// Columns encoded per step. A worker updates every recovery block one tile at a time, so the batch's
/// sources for the tile (`batch slices x TILE_BYTES`) stay cache-resident across all exponents. Over a
/// worker's whole range they did not: on an M4, 768 KB slices at 10% went from 51 s to 31 s of CPU
/// and 256 KiB slices from 91 s to 63 s, with no change at 10 MiB slices.
const TILE_BYTES: usize = 8 * 1024;
/// PAR2 2.0 has exactly 32768 distinct input-slice constants, so a set holds at most this many slices.
pub const MAX_INPUT_SLICES: usize = 32768;
/// Upper bound on concurrent quick-scan threads (a thread per file breaks large folders).
const MAX_SCAN_THREADS: usize = 16;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Par2Error {
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Io(String),
    #[error("context canceled")]
    Cancelled,
}

type Result<T> = std::result::Result<T, Par2Error>;

/// An on-disk input and the (relative, forward-slash) name recorded in its File Description packet;
/// an empty name means the file's base name.
#[derive(Debug, Clone)]
pub struct InputFile {
    pub path: PathBuf,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Slice size in bytes; a positive multiple of 4.
    pub slice_size: usize,
    pub num_recovery: usize,
    /// Encode worker threads (0 = all cores but one, which the reader and the sequential whole-file
    /// MD5 need: a worker on every core starves them and the whole pipeline waits on the MD5).
    pub num_threads: usize,
    /// Cap on recovery buffer memory (<= 0: 4 GiB).
    pub memory_limit: i64,
    /// Creator string embedded in the set (empty: "Postie").
    pub creator: String,
}

/// Encode-loop shape; fixed in production, varied by tests to prove it never changes the output.
#[derive(Debug, Clone, Copy)]
struct Tuning {
    /// Columns per encode step; a multiple of 4.
    tile: usize,
    /// Sources folded per kernel call: 1 means one source at a time, which the GF crate measures as
    /// faster on x86 without GFNI; otherwise the whole batch goes to the grouped kernel.
    batch_width: usize,
}

impl Tuning {
    fn native() -> Self {
        Self {
            tile: TILE_BYTES,
            batch_width: input_batch_width().max(1),
        }
    }
}

/// Encode workers for a requested count (0 = auto) on `available` cores.
fn worker_threads(requested: usize, available: usize) -> usize {
    if requested > 0 {
        return requested;
    }
    available.saturating_sub(1).max(1)
}

/// Receives `("hashing" | "encoding" | "writing", 0.0..=1.0)`.
pub type Progress<'a> = Option<&'a (dyn Fn(&str, f64) + Sync)>;

struct FileInfo {
    path: PathBuf,
    name: String,
    size: u64,
    hash16k: Md5Sum,
    hash_full: Md5Sum,
    id: Md5Sum,
    slices: Vec<IfscEntry>,
}

fn io_err(what: &str, path: &Path, e: &std::io::Error) -> Par2Error {
    Par2Error::Io(format!("{what} {}: {e}", path.display()))
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Ok(());
    }
    if name.contains('\\') {
        return Err(Par2Error::Invalid(format!(
            "name {name:?} must not contain backslash"
        )));
    }
    if name.starts_with('/') {
        return Err(Par2Error::Invalid(format!(
            "name {name:?} must be relative, not absolute"
        )));
    }
    if name.split('/').any(|s| s == "..") {
        return Err(Par2Error::Invalid(format!(
            "name {name:?} must not contain '..' segment"
        )));
    }
    Ok(())
}

fn num_slices(size: u64, slice_size: usize) -> usize {
    size.div_ceil(slice_size as u64) as usize
}

/// Quick-scans every input on at most `threads` (capped) worker threads, keeping input order.
fn scan_inputs(inputs: &[InputFile], threads: usize) -> Vec<Result<FileInfo>> {
    let workers = threads.clamp(1, MAX_SCAN_THREADS).min(inputs.len().max(1));
    let per_worker = inputs.len().div_ceil(workers).max(1);
    std::thread::scope(|scope| {
        let handles: Vec<_> = inputs
            .chunks(per_worker)
            .map(|chunk| scope.spawn(move || chunk.iter().map(quick_scan).collect::<Vec<_>>()))
            .collect();
        handles
            .into_iter()
            .zip(inputs.chunks(per_worker))
            .flat_map(|(h, chunk)| {
                h.join().unwrap_or_else(|_| {
                    chunk
                        .iter()
                        .map(|_| Err(Par2Error::Io("scan thread panicked".into())))
                        .collect()
                })
            })
            .collect()
    })
}

/// Size, hash of the first 16 KiB (the whole file when shorter) and file ID.
fn quick_scan(input: &InputFile) -> Result<FileInfo> {
    let mut f = File::open(&input.path).map_err(|e| io_err("open", &input.path, &e))?;
    let size = f
        .metadata()
        .map_err(|e| io_err("stat", &input.path, &e))?
        .len();
    let name = if input.name.is_empty() {
        input
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        input.name.clone()
    };
    let mut hash16k = [0u8; 16];
    if size > 0 {
        let mut buf = vec![0u8; size.min(HASH16K_LEN as u64) as usize];
        f.read_exact(&mut buf)
            .map_err(|e| io_err("read16k", &input.path, &e))?;
        hash16k = packets::md5_of(&[&buf]);
    }
    let id = packets::file_id(&hash16k, size, &name);
    Ok(FileInfo {
        path: input.path.clone(),
        name,
        size,
        hash16k,
        hash_full: [0; 16],
        id,
        slices: Vec::new(),
    })
}

/// One run of consecutive zero-padded slices of one file.
struct Batch {
    file: usize,
    /// Index of the first slice within its file.
    first: usize,
    /// Global (file-ID order) index of the first slice.
    global_first: usize,
    count: usize,
    /// Real (unpadded) bytes at the front of `data`.
    real: usize,
    data: Vec<u8>,
}

struct Plan<'a> {
    files: &'a [FileInfo],
    /// File indexes in file-ID order.
    order: &'a [usize],
    /// Global slice offset per file (by file index).
    offsets: &'a [usize],
    constants: &'a [u16],
    slice_size: usize,
    threads: usize,
    tuning: Tuning,
}

fn check(cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() {
        Err(Par2Error::Cancelled)
    } else {
        Ok(())
    }
}

/// Reads every file's batches in file-ID order.
fn read_batches(
    cancel: &CancellationToken,
    plan: &Plan<'_>,
    mut send: impl FnMut(Arc<Batch>) -> bool,
) -> Result<()> {
    let s = plan.slice_size;
    let per_batch = (BATCH_BYTES / s).clamp(1, MAX_BATCH_SLICES);
    for &fi in plan.order {
        let info = &plan.files[fi];
        if info.size == 0 {
            continue;
        }
        let mut f = File::open(&info.path).map_err(|e| io_err("open", &info.path, &e))?;
        let total = num_slices(info.size, s);
        let mut first = 0;
        let mut remaining = info.size;
        while first < total {
            check(cancel)?;
            let count = per_batch.min(total - first);
            let real = remaining.min((count * s) as u64) as usize;
            let mut data = vec![0u8; count * s];
            f.read_exact(&mut data[..real])
                .map_err(|e| io_err("read", &info.path, &e))?;
            remaining -= real as u64;
            let batch = Arc::new(Batch {
                file: fi,
                first,
                global_first: plan.offsets[fi] + first,
                count,
                real,
                data,
            });
            if !send(batch) {
                return Err(Par2Error::Cancelled);
            }
            first += count;
        }
    }
    Ok(())
}

fn column_ranges(slice_size: usize, threads: usize) -> Vec<(usize, usize)> {
    let width = slice_size.div_ceil(threads).div_ceil(COLUMN_ALIGN) * COLUMN_ALIGN;
    (0..threads)
        .map(|w| {
            (
                (w * width).min(slice_size),
                ((w + 1) * width).min(slice_size),
            )
        })
        .collect()
}

/// GF factor of every (exponent, batch slice) pair, row-major by exponent; shared by all workers.
fn batch_factors(batch: &Batch, plan: &Plan<'_>, exps: &[u32]) -> Vec<u16> {
    let constants = &plan.constants[batch.global_first..batch.global_first + batch.count];
    exps.iter()
        .flat_map(|&e| constants.iter().map(move |&c| gf::pow(c, e)))
        .collect()
}

/// Encodes one batch into this worker's column range of every recovery buffer, a tile at a time.
fn encode_columns(
    batch: &Batch,
    plan: &Plan<'_>,
    factors: &[u16],
    range: (usize, usize),
    acc: &mut [Vec<u8>],
) {
    let (a, b) = range;
    let s = plan.slice_size;
    let Tuning { tile, batch_width } = plan.tuning;
    let mut fs: Vec<FactorSrc<'_>> = Vec::with_capacity(batch.count);
    let mut t0 = a;
    while t0 < b {
        let t1 = t0.saturating_add(tile).min(b);
        let src = |j: usize| &batch.data[j * s + t0..j * s + t1];
        for (row, dst) in factors.chunks_exact(batch.count).zip(acc.iter_mut()) {
            let dst = &mut dst[t0 - a..t1 - a];
            if batch_width == 1 {
                for (j, &factor) in row.iter().enumerate() {
                    mul_acc_region(factor, src(j), dst);
                }
            } else {
                fs.clear();
                fs.extend(row.iter().enumerate().map(|(j, &factor)| FactorSrc {
                    factor,
                    src: src(j),
                }));
                mul_acc_input_batch(dst, &fs);
            }
        }
        t0 = t1;
    }
}

fn hash_slice(data: &[u8]) -> IfscEntry {
    let mut h = Md5::new();
    h.update(data);
    IfscEntry {
        md5: h.finalize().into(),
        crc32: crc32fast::hash(data),
    }
}

/// One pass over all inputs encoding `exps`; the first pass also computes the full-file MD5s and the
/// per-slice checksums. Returns the recovery blocks split by worker column range: `[worker][exp]`.
fn run_pass(
    cancel: &CancellationToken,
    plan: &Plan<'_>,
    files: &mut [FileInfo],
    exps: &[u32],
    first_pass: bool,
    progress: &(dyn Fn(f64) + Sync),
) -> Result<Vec<Vec<Vec<u8>>>> {
    let s = plan.slice_size;
    let ranges = column_ranges(s, plan.threads);
    let mut acc: Vec<Vec<Vec<u8>>> = ranges
        .iter()
        .map(|(a, b)| exps.iter().map(|_| vec![0u8; b - a]).collect())
        .collect();
    let total_slices: usize = plan.files.iter().map(|f| num_slices(f.size, s)).sum();
    let mut done = 0usize;

    let md5_results: Vec<(usize, Md5Sum)> =
        std::thread::scope(|scope| -> Result<Vec<(usize, Md5Sum)>> {
            // full-file MD5s (first pass only) on ONE worker: batches arrive in read order through a
            // FIFO, so a running hasher per file sees its bytes in sequence. A thread per file broke
            // folders with thousands of files (the OS thread limit).
            let (md5_tx, md5_handle) = if first_pass {
                let (tx, rx) = sync_channel::<Arc<Batch>>(4);
                let handle = scope.spawn(move || {
                    let mut hashers: std::collections::BTreeMap<usize, Md5> =
                        std::collections::BTreeMap::new();
                    while let Ok(b) = rx.recv() {
                        hashers.entry(b.file).or_default().update(&b.data[..b.real]);
                    }
                    hashers
                        .into_iter()
                        .map(|(fi, h)| (fi, <[u8; 16]>::from(h.finalize())))
                        .collect::<Vec<_>>()
                });
                (Some(tx), Some(handle))
            } else {
                (None, None)
            };
            let (tx, rx) = sync_channel::<Arc<Batch>>(2);
            let reader = scope.spawn(move || {
                let res = read_batches(cancel, plan, |b| {
                    if let Some(m) = &md5_tx
                        && m.send(b.clone()).is_err()
                    {
                        return false;
                    }
                    tx.send(b).is_ok()
                });
                drop(tx);
                res
            });

            let mut compute_err = None;
            for batch in rx.iter() {
                if let Err(e) = check(cancel) {
                    compute_err = Some(e);
                    break;
                }
                let factors = batch_factors(&batch, plan, exps);
                let entries: Vec<(usize, IfscEntry)> = std::thread::scope(|inner| {
                    let handles: Vec<_> = acc
                        .iter_mut()
                        .zip(&ranges)
                        .enumerate()
                        .map(|(w, (cols, range))| {
                            let (batch, factors) = (&batch, &factors);
                            inner.spawn(move || {
                                let mut out = Vec::new();
                                if first_pass {
                                    let mut j = w;
                                    while j < batch.count {
                                        out.push((
                                            batch.first + j,
                                            hash_slice(&batch.data[j * s..(j + 1) * s]),
                                        ));
                                        j += plan.threads;
                                    }
                                }
                                encode_columns(batch, plan, factors, *range, cols);
                                out
                            })
                        })
                        .collect();
                    handles
                        .into_iter()
                        .flat_map(|h| h.join().unwrap_or_default())
                        .collect()
                });
                if first_pass {
                    let slices = &mut files[batch.file].slices;
                    if slices.is_empty() {
                        slices.resize(
                            num_slices(plan.files[batch.file].size, s),
                            IfscEntry::default(),
                        );
                    }
                    for (idx, e) in entries {
                        slices[idx] = e;
                    }
                }
                done += batch.count;
                progress(done as f64 / total_slices.max(1) as f64);
            }
            // closing the receiver unblocks the reader if we stopped early
            drop(rx);
            let read_res = reader
                .join()
                .map_err(|_| Par2Error::Io("reader thread panicked".into()))?;
            let hashes = match md5_handle {
                Some(h) => h
                    .join()
                    .map_err(|_| Par2Error::Io("md5 thread panicked".into()))?,
                None => Vec::new(),
            };
            if let Some(e) = compute_err {
                return Err(e);
            }
            read_res?;
            Ok(hashes)
        })?;
    for (fi, h) in md5_results {
        files[fi].hash_full = h;
    }
    // the column ranges are written out as packet parts; stitching them would double peak memory
    Ok(acc)
}

fn write_main(
    path: &Path,
    set_id: &Md5Sum,
    main: &[u8],
    files: &[FileInfo],
    creator: &str,
) -> std::io::Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    packets::write_packet(&mut w, set_id, &packets::TYPE_MAIN, main)?;
    packets::write_packet(
        &mut w,
        set_id,
        &packets::TYPE_CREATOR,
        &packets::creator_body(creator),
    )?;
    for f in files {
        packets::write_packet(
            &mut w,
            set_id,
            &packets::TYPE_FILE_DESC,
            &packets::file_desc_body(&f.id, &f.hash_full, &f.hash16k, f.size, &f.name),
        )?;
        packets::write_packet(
            &mut w,
            set_id,
            &packets::TYPE_IFSC,
            &packets::ifsc_body(&f.id, &f.slices),
        )?;
    }
    w.flush()
}

/// Volume layout: block counts 1, 1, 2, 4, 8, ... named `<base>.vol<offset>+<count>.par2`.
fn volume_groups(total: usize) -> Vec<(usize, usize)> {
    let (mut groups, mut offset, mut count, mut first) = (Vec::new(), 0, 1, true);
    while offset < total {
        let n = count.min(total - offset);
        groups.push((offset, n));
        offset += n;
        if first {
            first = false;
        } else {
            count *= 2;
        }
    }
    groups
}

/// Recovery volume files, filled as each encoding pass finishes so the recovery blocks of a large
/// set never have to be held in memory all at once. Passes produce ascending exponents, so every
/// volume receives its blocks in order.
struct VolumeWriters {
    set_id: Md5Sum,
    groups: Vec<(usize, usize)>,
    paths: Vec<PathBuf>,
    writers: Vec<BufWriter<File>>,
}

impl VolumeWriters {
    fn create(output: &Path, set_id: &Md5Sum, total: usize) -> Result<Self> {
        let name = output
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let base = match name.rfind('.') {
            Some(i) => name[..i].to_string(),
            None => name,
        };
        let dir = output.parent().map(Path::to_path_buf).unwrap_or_default();
        let groups = volume_groups(total);
        let mut this = Self {
            set_id: *set_id,
            groups,
            paths: Vec::new(),
            writers: Vec::new(),
        };
        for &(offset, n) in &this.groups.clone() {
            let path = dir.join(format!("{base}.vol{offset:02}+{n:02}.par2"));
            match File::create(&path) {
                Ok(f) => {
                    this.paths.push(path);
                    this.writers.push(BufWriter::new(f));
                }
                Err(e) => {
                    this.discard();
                    return Err(Par2Error::Io(format!(
                        "par2go: writing volume files failed: {}: {e}",
                        path.display()
                    )));
                }
            }
        }
        Ok(this)
    }

    /// Appends the recovery blocks for exponents `first..first + count`, each held as one piece per
    /// worker column range (`cols[worker][i]`).
    fn write_chunk(&mut self, first: usize, count: usize, cols: &[Vec<Vec<u8>>]) -> Result<()> {
        for i in 0..count {
            let exp = first + i;
            let g = self.groups.partition_point(|&(offset, _)| offset <= exp) - 1;
            let exp_bytes = (exp as u32).to_le_bytes();
            let mut parts: Vec<&[u8]> = Vec::with_capacity(cols.len() + 1);
            parts.push(&exp_bytes);
            parts.extend(cols.iter().map(|c| c[i].as_slice()));
            packets::write_packet_parts(
                &mut self.writers[g],
                &self.set_id,
                &packets::TYPE_RECOVERY,
                &parts,
            )
            .map_err(|e| {
                Par2Error::Io(format!(
                    "par2go: writing volume files failed: {}: {e}",
                    self.paths[g].display()
                ))
            })?;
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<()> {
        for (w, path) in self.writers.iter_mut().zip(&self.paths) {
            w.flush().map_err(|e| {
                Par2Error::Io(format!(
                    "par2go: writing volume files failed: {}: {e}",
                    path.display()
                ))
            })?;
        }
        Ok(())
    }

    /// Removes the partially written volumes (cancel or failure).
    fn discard(&mut self) {
        self.writers.clear();
        for p in self.paths.drain(..) {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// Creates a PAR2 set: `output` (the index file with no recovery data) plus the recovery volumes next
/// to it.
pub fn create_with_names(
    cancel: &CancellationToken,
    output: &Path,
    inputs: &[InputFile],
    opts: &Options,
    progress: Progress<'_>,
) -> Result<()> {
    create_tuned(cancel, output, inputs, opts, progress, Tuning::native())
}

fn create_tuned(
    cancel: &CancellationToken,
    output: &Path,
    inputs: &[InputFile],
    opts: &Options,
    progress: Progress<'_>,
    tuning: Tuning,
) -> Result<()> {
    if opts.slice_size == 0 || !opts.slice_size.is_multiple_of(4) {
        return Err(Par2Error::Invalid(format!(
            "par2go: SliceSize must be a positive multiple of 4, got {}",
            opts.slice_size
        )));
    }
    if opts.num_recovery == 0 {
        return Err(Par2Error::Invalid(
            "par2go: NumRecovery must be positive, got 0".into(),
        ));
    }
    if inputs.is_empty() {
        return Err(Par2Error::Invalid("par2go: no input files".into()));
    }
    for (i, input) in inputs.iter().enumerate() {
        if input.path.as_os_str().is_empty() {
            return Err(Par2Error::Invalid(format!(
                "par2go: input {i} has empty path"
            )));
        }
        validate_name(&input.name).map_err(|e| {
            Par2Error::Invalid(format!("par2go: input {i} ({}): {e}", input.path.display()))
        })?;
    }
    let report = |phase: &str, pct: f64| {
        if let Some(p) = progress {
            p(phase, pct);
        }
    };
    let threads = worker_threads(
        opts.num_threads,
        std::thread::available_parallelism().map_or(1, usize::from),
    );
    let memory_limit = if opts.memory_limit <= 0 {
        DEFAULT_MEMORY_LIMIT
    } else {
        opts.memory_limit
    };
    let creator = if opts.creator.is_empty() {
        "Postie"
    } else {
        &opts.creator
    };
    report("hashing", 0.0);

    let scanned = scan_inputs(inputs, threads);
    check(cancel)?;
    let mut files = scanned
        .into_iter()
        .collect::<Result<Vec<_>>>()
        .map_err(|e| match e {
            Par2Error::Io(m) => Par2Error::Io(format!("par2go: scanning failed: {m}")),
            other => other,
        })?;

    let mut ids: Vec<Md5Sum> = files.iter().map(|f| f.id).collect();
    ids.sort_unstable();
    let main = packets::main_body(opts.slice_size as u64, &ids);
    let set_id = packets::recovery_set_id(&main);

    // source slices are numbered in file-ID order
    let mut order: Vec<usize> = (0..files.len()).collect();
    order.sort_by_key(|&i| files[i].id);
    let mut offsets = vec![0usize; files.len()];
    let mut total_slices = 0;
    for &i in &order {
        offsets[i] = total_slices;
        total_slices += num_slices(files[i].size, opts.slice_size);
    }
    if total_slices == 0 {
        return Err(Par2Error::Invalid(
            "par2go: no input slices (all files empty?)".into(),
        ));
    }
    if total_slices > MAX_INPUT_SLICES {
        return Err(Par2Error::Invalid(format!(
            "par2go: {total_slices} input slices exceed the PAR2 limit of {MAX_INPUT_SLICES}; use a larger slice size or fewer files"
        )));
    }
    let constants = gf::input_slice_constants(total_slices);
    let per_chunk =
        ((memory_limit / opts.slice_size as i64).max(1) as usize).min(opts.num_recovery);
    let exps: Vec<u32> = (0..opts.num_recovery as u32).collect();
    let chunks: Vec<&[u32]> = exps.chunks(per_chunk).collect();

    // the plan borrows the file table immutably while a pass records hashes into `files`
    let snapshot: Vec<FileInfo> = files
        .iter()
        .map(|f| FileInfo {
            path: f.path.clone(),
            name: f.name.clone(),
            size: f.size,
            hash16k: f.hash16k,
            hash_full: f.hash_full,
            id: f.id,
            slices: Vec::new(),
        })
        .collect();
    let plan = Plan {
        files: &snapshot,
        order: &order,
        offsets: &offsets,
        constants: &constants,
        slice_size: opts.slice_size,
        threads,
        tuning,
    };
    let mut volumes = VolumeWriters::create(output, &set_id, opts.num_recovery)?;
    let mut first_exp = 0usize;
    for (ci, chunk) in chunks.iter().enumerate() {
        let pass = (|| -> Result<()> {
            check(cancel)?;
            let base = ci as f64 / chunks.len() as f64;
            let scale = 1.0 / chunks.len() as f64;
            let on_progress = |p: f64| report("hashing", base + p * scale);
            let cols = run_pass(cancel, &plan, &mut files, chunk, ci == 0, &on_progress)?;
            // this pass's recovery blocks go to disk now and are dropped before the next pass
            volumes.write_chunk(first_exp, chunk.len(), &cols)
        })();
        if let Err(e) = pass {
            volumes.discard();
            return Err(e);
        }
        first_exp += chunk.len();
    }
    report("hashing", 1.0);
    report("encoding", 1.0);

    report("writing", 0.0);
    let finished = write_main(output, &set_id, &main, &files, creator)
        .map_err(|e| Par2Error::Io(format!("par2go: writing main file failed: {e}")))
        .and_then(|()| volumes.finish());
    if let Err(e) = finished {
        // leave no partial set behind: the index file and every volume
        volumes.discard();
        let _ = std::fs::remove_file(output);
        return Err(e);
    }
    report("writing", 1.0);
    Ok(())
}

#[cfg(test)]
mod tests;
