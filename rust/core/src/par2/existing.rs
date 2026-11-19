//! Port of `internal/par2/existing.go`: finding, fingerprinting and reusing PAR2 sets already on disk.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

use crate::config::Par2Config;
use crate::fileinfo::FileInfo;

use super::create::InputFile;
use super::packets::{self, HEADER_SIZE, MAGIC, TYPE_FILE_DESC};

/// Directory Postie creates inside `par2.temp_dir` for the PAR2 files it generates, so the sweeper can
/// remove orphans without touching anything that is not Postie's.
pub const WORK_SUBDIR: &str = "postie-par2";
const FILE_DESC_MIN_LEN: u64 = 16 + 16 + 16 + 8;
/// Bounds a single packet while scanning so a corrupt length cannot make us allocate or seek absurdly.
const MAX_PACKET_LEN: u64 = 1 << 30;
const HASH16K_LEN: u64 = 16384;

/// Where generated PAR2 files go when `temp_dir` is configured, or `""` to write next to the sources.
pub fn work_dir(cfg: &Par2Config) -> String {
    if cfg.temp_dir.is_empty() {
        String::new()
    } else {
        Path::new(&cfg.temp_dir)
            .join(WORK_SUBDIR)
            .to_string_lossy()
            .into_owned()
    }
}

/// Outcome of a PAR2 request. `created` lists files this call wrote, which Postie owns and may delete;
/// `reused` lists PAR2 files that already existed (the user's own, or a verified leftover), which are
/// posted alongside the sources but are never Postie's to delete.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Par2Result {
    pub created: Vec<String>,
    pub reused: Vec<String>,
}

impl Par2Result {
    /// Every PAR2 file to post, reused first.
    pub fn all(&self) -> Vec<String> {
        self.reused.iter().chain(&self.created).cloned().collect()
    }
}

/// What a File Description packet records: name, length and the MD5 of the first 16 KiB.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDesc {
    pub name: String,
    pub size: u64,
    pub hash16k: [u8; 16],
}

/// True for `.volN+M.par2` — the whole suffix after the set's base name, so a set whose name merely
/// starts with ours (`movie.mkv.1080p.vol00+01.par2`) never matches.
fn is_volume_suffix(s: &str) -> bool {
    let Some(rest) = s.strip_prefix(".vol").and_then(|r| r.strip_suffix(".par2")) else {
        return false;
    };
    rest.split_once('+').is_some_and(|(a, b)| {
        !a.is_empty()
            && !b.is_empty()
            && a.bytes().all(|c| c.is_ascii_digit())
            && b.bytes().all(|c| c.is_ascii_digit())
    })
}

/// The main PAR2 file plus every `<base>.volN+M.par2` of the set named `base` in `dir`.
pub fn par2_set_on_disk(dir: &Path, base: &str) -> Vec<String> {
    let mut out = vec![
        dir.join(format!("{base}.par2"))
            .to_string_lossy()
            .into_owned(),
    ];
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            eprintln!(
                "par2: failed to read directory for par2 volumes: dir={} error={e}",
                dir.display()
            );
            return out;
        }
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_dir()) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.strip_prefix(base).is_some_and(is_volume_suffix) {
            out.push(dir.join(&name).to_string_lossy().into_owned());
        }
    }
    out
}

/// Parses the File Description packets of a PAR2 file (metadata only; the main file has no recovery data).
pub fn read_file_descs(path: &Path) -> Result<Vec<FileDesc>, String> {
    let name = path.display();
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let mut descs = Vec::new();
    while let Some(header) =
        read_header(&mut f).map_err(|e| format!("par2 {name}: read packet header: {e}"))?
    {
        if header[..8] != MAGIC {
            return Err(format!("par2 {name}: bad packet magic"));
        }
        let length = u64::from_le_bytes(header[8..16].try_into().unwrap_or_default());
        if !(HEADER_SIZE as u64..=MAX_PACKET_LEN).contains(&length) {
            return Err(format!("par2 {name}: implausible packet length {length}"));
        }
        let body_len = length - HEADER_SIZE as u64;
        if header[48..64] != TYPE_FILE_DESC {
            f.seek(SeekFrom::Current(body_len as i64))
                .map_err(|e| format!("par2 {name}: skip packet: {e}"))?;
            continue;
        }
        if body_len < FILE_DESC_MIN_LEN {
            return Err(format!(
                "par2 {name}: FileDesc packet too short ({body_len} bytes)"
            ));
        }
        let mut body = vec![0u8; body_len as usize];
        f.read_exact(&mut body)
            .map_err(|e| format!("par2 {name}: read FileDesc: {e}"))?;
        let mut hash16k = [0u8; 16];
        hash16k.copy_from_slice(&body[32..48]);
        let size = u64::from_le_bytes(body[48..56].try_into().unwrap_or_default());
        let end = body[56..]
            .iter()
            .rposition(|b| *b != 0)
            .map_or(0, |i| i + 1);
        descs.push(FileDesc {
            name: String::from_utf8_lossy(&body[56..56 + end]).into_owned(),
            size,
            hash16k,
        });
    }
    if descs.is_empty() {
        return Err(format!("par2 {name}: no FileDesc packets"));
    }
    Ok(descs)
}

/// Reads one packet header: `None` on a clean end of file, an error on a partial header (Go's
/// `io.EOF` versus `io.ErrUnexpectedEOF`).
fn read_header(f: &mut File) -> std::io::Result<Option<[u8; HEADER_SIZE]>> {
    let mut header = [0u8; HEADER_SIZE];
    let mut filled = 0;
    while filled < HEADER_SIZE {
        match f.read(&mut header[filled..])? {
            0 if filled == 0 => return Ok(None),
            0 => return Err(std::io::ErrorKind::UnexpectedEof.into()),
            n => filled += n,
        }
    }
    Ok(Some(header))
}

/// The File Description a creator would record for `path` under `name`.
pub fn describe_source(path: &Path, name: &str) -> Result<FileDesc, String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let size = f.metadata().map_err(|e| e.to_string())?.len();
    let mut d = FileDesc {
        name: name.into(),
        size,
        hash16k: [0; 16],
    };
    if size == 0 {
        return Ok(d);
    }
    let mut buf = vec![0u8; size.min(HASH16K_LEN) as usize];
    f.read_exact(&mut buf)
        .map_err(|e| format!("read first 16k of {}: {e}", path.display()))?;
    d.hash16k = packets::md5_of(&[&buf]);
    Ok(d)
}

/// True when the set whose main file is `main_path` was built from exactly `inputs`: every input must
/// appear with the same name, size and leading hash, and the set must not describe anything else. A set
/// that fails this belongs to other data and must not be attached to this upload.
pub fn par2_describes(main_path: &Path, inputs: &[InputFile]) -> bool {
    let descs = match read_file_descs(main_path) {
        Ok(d) => d,
        Err(e) => {
            eprintln!(
                "par2: ignoring unreadable PAR2 file: path={} error={e}",
                main_path.display()
            );
            return false;
        }
    };
    if descs.len() != inputs.len() {
        eprintln!(
            "par2: ignoring PAR2 set that describes a different number of files: path={} par2Files={} inputFiles={}",
            main_path.display(),
            descs.len(),
            inputs.len()
        );
        return false;
    }
    let by_name: std::collections::HashMap<&str, &FileDesc> =
        descs.iter().map(|d| (d.name.as_str(), d)).collect();
    for input in inputs {
        let want = match describe_source(&input.path, &input.name) {
            Ok(w) => w,
            Err(e) => {
                eprintln!(
                    "par2: cannot fingerprint source file for PAR2 reuse: path={} error={e}",
                    input.path.display()
                );
                return false;
            }
        };
        if by_name
            .get(input.name.as_str())
            .is_none_or(|got| **got != want)
        {
            eprintln!(
                "par2: ignoring PAR2 set that does not describe the source file: path={} sourceFile={}",
                main_path.display(),
                input.path.display()
            );
            return false;
        }
    }
    true
}

fn base_name(p: &str) -> String {
    Path::new(p)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Reuses `<dir>/<base>.par2` for `source` only when it describes exactly that file.
pub fn check_existing_par2_files_in_path(source: &FileInfo, dir: &Path) -> Option<Vec<String>> {
    let base = base_name(&source.path);
    let main = dir.join(format!("{base}.par2"));
    if !main.exists() {
        return None;
    }
    if !par2_describes(
        &main,
        &[InputFile {
            path: PathBuf::from(&source.path),
            name: base.clone(),
        }],
    ) {
        return None;
    }
    let set = par2_set_on_disk(dir, &base);
    eprintln!(
        "par2: found existing PAR2 files, skipping generation: sourceFile={} par2Files={}",
        source.path,
        set.len()
    );
    Some(set)
}

/// Reuses `<dir>/<set_name>.par2` only when it describes exactly `inputs`.
pub fn check_existing_par2_set_in_path(
    set_name: &str,
    dir: &Path,
    inputs: &[InputFile],
) -> Option<Vec<String>> {
    let main = dir.join(format!("{set_name}.par2"));
    if !main.exists() || !par2_describes(&main, inputs) {
        return None;
    }
    let set = par2_set_on_disk(dir, set_name);
    eprintln!(
        "par2: found existing PAR2 set, skipping generation: setName={set_name} par2Files={}",
        set.len()
    );
    Some(set)
}

/// `/`-separated path of `target` relative to `base`, or `None` when it cannot be expressed.
fn rel(base: &Path, target: &Path) -> Option<String> {
    let comps: Vec<Component<'_>> = target.strip_prefix(base).ok()?.components().collect();
    (!comps.is_empty()).then(|| {
        comps
            .iter()
            .map(|c| c.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/")
    })
}

/// Names recorded in a folder set's FileDesc packets: the path relative to `folder_dir` with forward
/// slashes (the job folder a downloader creates is already named after the NZB), else the base name.
pub fn set_input_names(inputs: &[FileInfo], folder_dir: &str) -> Vec<InputFile> {
    inputs
        .iter()
        .map(|f| InputFile {
            path: PathBuf::from(&f.path),
            name: rel(Path::new(folder_dir), Path::new(&f.path))
                .unwrap_or_else(|| base_name(&f.path)),
        })
        .collect()
}

/// The source whose per-file set `par2_path` belongs to (`<dir>/<base>` for `<dir>/<base>.par2` or
/// `<dir>/<base>.volN+M.par2`); `None` when it is not a PAR2 name.
pub fn set_source_for(par2_path: &str) -> Option<String> {
    let p = Path::new(par2_path);
    let name = p.file_name()?.to_string_lossy().into_owned();
    let stem = name.strip_suffix(".par2")?;
    // lazy `(.+?)` then an optional `.volN+M`: the shortest base wins
    let base = match stem.rfind(".vol") {
        Some(i) if i > 0 && is_volume_suffix(&format!("{}.par2", &stem[i..])) => &stem[..i],
        _ => stem,
    };
    (!base.is_empty()).then(|| {
        p.parent()
            .map(|d| d.join(base))
            .unwrap_or_else(|| PathBuf::from(base))
            .to_string_lossy()
            .into_owned()
    })
}

/// The PAR2 set next to `source_path` that one-NZB-per-file posting reuses instead of generating
/// parity; the same check as `create_in_directory`.
pub fn reusable_set_for(source_path: &str) -> Option<Vec<String>> {
    let dir = Path::new(source_path).parent()?.to_path_buf();
    check_existing_par2_files_in_path(
        &FileInfo {
            path: source_path.into(),
            ..Default::default()
        },
        &dir,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::par2::create::{Options, create_with_names};
    use tokio_util::sync::CancellationToken;

    fn tmp() -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "par2ex-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn make_set(dir: &Path, files: &[(&str, &[u8])], set: &str) -> Vec<InputFile> {
        let inputs: Vec<InputFile> = files
            .iter()
            .map(|(n, d)| {
                let p = dir.join(Path::new(n).file_name().unwrap());
                std::fs::write(&p, d).unwrap();
                InputFile {
                    path: p,
                    name: (*n).into(),
                }
            })
            .collect();
        let o = Options {
            slice_size: 4096,
            num_recovery: 2,
            num_threads: 1,
            memory_limit: 0,
            creator: "Postie".into(),
        };
        create_with_names(
            &CancellationToken::new(),
            &dir.join(format!("{set}.par2")),
            &inputs,
            &o,
            None,
        )
        .unwrap();
        inputs
    }

    #[test]
    fn volume_suffix_matching_is_anchored() {
        for ok in [".vol00+01.par2", ".vol1+22.par2", ".vol100+4.par2"] {
            assert!(is_volume_suffix(ok), "{ok}");
        }
        for bad in [
            ".vol+1.par2",
            ".vol1+.par2",
            ".volx+1.par2",
            ".vol00+01.par2x",
            ".par2",
            ".1080p.vol00+01.par2",
            "",
        ] {
            assert!(!is_volume_suffix(bad), "{bad}");
        }
    }

    #[test]
    fn set_on_disk_lists_main_and_only_its_own_volumes() {
        let d = tmp();
        for n in [
            "movie.mkv.par2",
            "movie.mkv.vol00+01.par2",
            "movie.mkv.vol01+02.par2",
            "movie.mkv.1080p.vol00+01.par2",
            "other.par2",
            "movie.mkv.vol00+01.par2.bak",
        ] {
            std::fs::write(d.join(n), b"x").unwrap();
        }
        std::fs::create_dir(d.join("movie.mkv.vol09+09.par2")).unwrap();
        let mut got: Vec<String> = par2_set_on_disk(&d, "movie.mkv")
            .iter()
            .map(|p| base_name(p))
            .collect();
        got.sort();
        assert_eq!(
            got,
            [
                "movie.mkv.par2",
                "movie.mkv.vol00+01.par2",
                "movie.mkv.vol01+02.par2"
            ]
        );
    }

    #[test]
    fn file_descs_round_trip_with_the_creator_and_describe_matches_only_the_same_data() {
        let d = tmp();
        let data: Vec<u8> = (0..9000u32).map(|i| (i * 7) as u8).collect();
        let inputs = make_set(&d, &[("a.bin", &data)], "a.bin");
        let descs = read_file_descs(&d.join("a.bin.par2")).unwrap();
        assert_eq!(descs, [describe_source(&inputs[0].path, "a.bin").unwrap()]);
        assert!(par2_describes(&d.join("a.bin.par2"), &inputs));
        // changed content (first 16 KiB differs), changed name and an extra input all break reuse
        let mut other = data.clone();
        other[0] ^= 1;
        std::fs::write(&inputs[0].path, &other).unwrap();
        assert!(!par2_describes(&d.join("a.bin.par2"), &inputs));
        std::fs::write(&inputs[0].path, &data).unwrap();
        assert!(!par2_describes(
            &d.join("a.bin.par2"),
            &[InputFile {
                name: "renamed.bin".into(),
                ..inputs[0].clone()
            }]
        ));
        assert!(!par2_describes(
            &d.join("a.bin.par2"),
            &[inputs[0].clone(), inputs[0].clone()]
        ));
    }

    #[test]
    fn unreadable_or_corrupt_par2_files_are_ignored() {
        let d = tmp();
        let src = d.join("s.bin");
        std::fs::write(&src, b"hello").unwrap();
        let input = InputFile {
            path: src,
            name: "s.bin".into(),
        };
        std::fs::write(
            d.join("junk.par2"),
            b"not a par2 file at all, but longer than a header....................",
        )
        .unwrap();
        assert!(!par2_describes(
            &d.join("junk.par2"),
            std::slice::from_ref(&input)
        ));
        std::fs::write(d.join("empty.par2"), b"").unwrap();
        assert!(
            read_file_descs(&d.join("empty.par2"))
                .unwrap_err()
                .contains("no FileDesc packets")
        );
        assert!(read_file_descs(&d.join("missing.par2")).is_err());
        // a partial header is an error, a clean end after whole packets is not
        let whole = std::fs::read(make_set_path(&d)).unwrap();
        let first_len = u64::from_le_bytes(whole[8..16].try_into().unwrap()) as usize;
        let mut cut = whole[..first_len + 20].to_vec();
        std::fs::write(d.join("trunc.par2"), &cut).unwrap();
        assert!(
            read_file_descs(&d.join("trunc.par2"))
                .unwrap_err()
                .contains("read packet header")
        );
        cut.truncate(first_len);
        std::fs::write(d.join("clean.par2"), &cut).unwrap();
        assert!(
            read_file_descs(&d.join("clean.par2"))
                .unwrap_err()
                .contains("no FileDesc packets"),
            "clean end, but no descs in a main-only prefix"
        );
    }

    fn make_set_path(d: &Path) -> PathBuf {
        make_set(d, &[("t.bin", b"abcdefgh")], "t.bin");
        d.join("t.bin.par2")
    }

    #[test]
    fn existing_checks_for_single_files_and_sets() {
        let d = tmp();
        let inputs = make_set(&d, &[("a.bin", b"aaaa")], "a.bin");
        let fi = FileInfo {
            path: inputs[0].path.to_string_lossy().into_owned(),
            ..Default::default()
        };
        let found = check_existing_par2_files_in_path(&fi, &d).unwrap();
        assert!(found.len() >= 2 && found[0].ends_with("a.bin.par2"));
        assert!(
            check_existing_par2_files_in_path(
                &FileInfo {
                    path: d.join("zzz").to_string_lossy().into_owned(),
                    ..Default::default()
                },
                &d
            )
            .is_none()
        );
        let set_inputs = make_set(&d, &[("x.bin", b"xxxx"), ("y.bin", b"yyyyyy")], "myset");
        assert!(check_existing_par2_set_in_path("myset", &d, &set_inputs).is_some());
        assert!(
            check_existing_par2_set_in_path("myset", &d, &set_inputs[..1]).is_none(),
            "a subset is a different set"
        );
        assert!(check_existing_par2_set_in_path("nope", &d, &set_inputs).is_none());
        assert!(reusable_set_for(&fi.path).is_some());
    }

    #[test]
    fn set_input_names_are_relative_and_slashed_with_a_basename_fallback() {
        let files = [
            FileInfo {
                path: "/root/Show/extras/bonus.mkv".into(),
                ..Default::default()
            },
            FileInfo {
                path: "/root/Show/ep.mkv".into(),
                ..Default::default()
            },
            FileInfo {
                path: "/elsewhere/z.mkv".into(),
                ..Default::default()
            },
        ];
        let names: Vec<String> = set_input_names(&files, "/root/Show")
            .into_iter()
            .map(|i| i.name)
            .collect();
        assert_eq!(names, ["extras/bonus.mkv", "ep.mkv", "z.mkv"]);
    }

    #[cfg(unix)] // fixture uses Unix paths or sh
    #[test]
    fn set_source_for_strips_the_par2_suffixes() {
        assert_eq!(
            set_source_for("/d/movie.rar.par2").as_deref(),
            Some("/d/movie.rar")
        );
        assert_eq!(
            set_source_for("/d/movie.rar.vol00+01.par2").as_deref(),
            Some("/d/movie.rar")
        );
        assert_eq!(
            set_source_for("/d/a.vol1+2.vol3+4.par2").as_deref(),
            Some("/d/a.vol1+2")
        );
        assert_eq!(set_source_for("/d/movie.mkv"), None);
        assert_eq!(set_source_for("/d/.par2"), None);
    }

    #[cfg(unix)] // fixture uses Unix paths or sh
    #[test]
    fn work_dir_and_result_ordering() {
        let mut cfg = Par2Config::default();
        assert_eq!(work_dir(&cfg), "");
        cfg.temp_dir = "/tmp/x".into();
        assert_eq!(work_dir(&cfg), "/tmp/x/postie-par2");
        let r = Par2Result {
            created: vec!["c".into()],
            reused: vec!["r".into()],
        };
        assert_eq!(r.all(), ["r", "c"]);
    }
}
