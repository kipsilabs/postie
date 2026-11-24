//! A downloader written independently of Postie: it reads an NZB the way a newsreader does, takes
//! each segment from what the fake server stored, decodes the yEnc, rebuilds the files and checks them
//! against the originals. It deliberately shares no code with `postie_core::nzb` or `postie-yenc`, so
//! a bug there cannot verify itself.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use md5::{Digest, Md5};

use super::Fake;

/// One `<file>` of an NZB: its declared size and segments by number.
#[derive(Debug)]
pub struct NzbFile {
    pub subject: String,
    pub bytes: u64,
    pub segments: Vec<(u32, String)>,
}

/// A file rebuilt from its articles.
#[derive(Debug, Clone)]
pub struct Decoded {
    /// The `name=` of its `=ybegin` lines.
    pub yenc_name: String,
    pub data: Vec<u8>,
}

impl Decoded {
    pub fn md5(&self) -> [u8; 16] {
        md5_of(&self.data)
    }
    pub fn is_par2(&self) -> bool {
        self.data.starts_with(PAR2_MAGIC)
    }
}

pub fn md5_of(b: &[u8]) -> [u8; 16] {
    Md5::digest(b).into()
}

fn attr(tag: &str, name: &str) -> Option<String> {
    let key = format!("{name}=\"");
    let start = tag.find(&key)? + key.len();
    let end = start + tag[start..].find('"')?;
    Some(unescape(&tag[start..end]))
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Parses the `<file>` entries of an NZB.
pub fn parse_nzb(xml: &str) -> Vec<NzbFile> {
    let mut files = Vec::new();
    let mut rest = xml;
    while let Some(i) = rest.find("<file ") {
        rest = &rest[i..];
        let tag_end = rest.find('>').expect("unterminated <file>");
        let tag = &rest[..tag_end];
        let end = rest.find("</file>").expect("missing </file>");
        let body = &rest[..end];
        let mut segments = Vec::new();
        let mut seg_rest = body;
        while let Some(j) = seg_rest.find("<segment ") {
            seg_rest = &seg_rest[j..];
            let open_end = seg_rest.find('>').expect("unterminated <segment>");
            let close = seg_rest.find("</segment>").expect("missing </segment>");
            let number = attr(&seg_rest[..open_end], "number")
                .and_then(|n| n.parse().ok())
                .expect("segment number");
            let id = unescape(seg_rest[open_end + 1..close].trim());
            segments.push((number, id));
            seg_rest = &seg_rest[close..];
        }
        files.push(NzbFile {
            subject: attr(tag, "subject").unwrap_or_default(),
            bytes: attr(tag, "bytes").and_then(|b| b.parse().ok()).unwrap_or(0),
            segments,
        });
        rest = &rest[end + "</file>".len()..];
    }
    files
}

/// Reads an NZB from disk, unpacking it when it is compressed.
pub fn read_nzb(path: &Path) -> String {
    let raw = std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    String::from_utf8(raw).unwrap_or_else(|_| panic!("{} is not plain XML", path.display()))
}

fn kv(line: &str, key: &str) -> Option<String> {
    // `name=` is always last on =ybegin and may contain spaces
    if key == "name" {
        return line.find(" name=").map(|i| line[i + 6..].to_string());
    }
    line.split_whitespace()
        .find_map(|t| t.strip_prefix(&format!("{key}=")).map(String::from))
}

/// One decoded article: its byte range in the file (1-based, inclusive) and payload.
struct Part {
    name: String,
    file_size: u64,
    begin: u64,
    end: u64,
    data: Vec<u8>,
}

/// Decodes one stored article (headers, blank line, yEnc). Panics with `id` on any malformation.
fn decode_article(id: &str, raw: &[u8]) -> Part {
    let sep = raw
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .unwrap_or_else(|| panic!("<{id}>: no header/body separator"));
    let body = &raw[sep + 4..];
    let mut lines = body
        .split(|&b| b == b'\n')
        .map(|l| l.strip_suffix(b"\r").unwrap_or(l));
    let ybegin = String::from_utf8_lossy(lines.next().unwrap_or_default()).into_owned();
    assert!(
        ybegin.starts_with("=ybegin "),
        "<{id}>: body starts with {ybegin:?}"
    );
    let ypart = String::from_utf8_lossy(lines.next().unwrap_or_default()).into_owned();
    assert!(
        ypart.starts_with("=ypart "),
        "<{id}>: no =ypart, got {ypart:?}"
    );
    let num = |line: &str, k: &str| -> u64 {
        kv(line, k)
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("<{id}>: no {k}= in {line:?}"))
    };
    let (begin, end) = (num(&ypart, "begin"), num(&ypart, "end"));
    let mut data = Vec::with_capacity((end + 1 - begin) as usize);
    let mut yend = None;
    for line in lines {
        if line.starts_with(b"=yend") {
            yend = Some(String::from_utf8_lossy(line).into_owned());
            break;
        }
        let mut it = line.iter();
        while let Some(&c) = it.next() {
            if c == b'=' {
                let n = *it
                    .next()
                    .unwrap_or_else(|| panic!("<{id}>: escape at end of line"));
                data.push(n.wrapping_sub(64).wrapping_sub(42));
            } else {
                data.push(c.wrapping_sub(42));
            }
        }
    }
    let yend = yend.unwrap_or_else(|| panic!("<{id}>: no =yend"));
    assert_eq!(num(&yend, "size"), data.len() as u64, "<{id}>: =yend size");
    assert_eq!(
        end + 1 - begin,
        data.len() as u64,
        "<{id}>: =ypart range vs data"
    );
    let pcrc = kv(&yend, "pcrc32").unwrap_or_else(|| panic!("<{id}>: no pcrc32"));
    assert_eq!(
        u32::from_str_radix(&pcrc, 16).unwrap_or_else(|_| panic!("<{id}>: pcrc32 {pcrc}")),
        crc32fast::hash(&data),
        "<{id}>: part CRC mismatch"
    );
    Part {
        name: kv(&ybegin, "name").unwrap_or_default(),
        file_size: num(&ybegin, "size"),
        begin,
        end,
        data,
    }
}

/// Rebuilds every file of an NZB from the fake's store, checking segment numbering, byte ranges and
/// CRCs. Panics naming the first problem, like a newsreader reporting a broken download.
pub fn download(nzb_xml: &str, fake: &Fake) -> Vec<Decoded> {
    let files = parse_nzb(nzb_xml);
    assert!(!files.is_empty(), "the NZB lists no files:\n{nzb_xml}");
    files
        .iter()
        .map(|f| {
            let mut nums: Vec<u32> = f.segments.iter().map(|s| s.0).collect();
            nums.sort_unstable();
            let want: Vec<u32> = (1..=f.segments.len() as u32).collect();
            assert_eq!(nums, want, "{}: segments are not numbered 1..N", f.subject);
            let mut segs = f.segments.clone();
            segs.sort_by_key(|s| s.0);
            let mut out = Vec::new();
            let mut name = String::new();
            let mut size = 0;
            for (n, id) in &segs {
                let raw = fake.article(id).unwrap_or_else(|| {
                    panic!("{} segment {n}: <{id}> is not on the server", f.subject)
                });
                let p = decode_article(id, &raw);
                assert_eq!(
                    p.begin,
                    out.len() as u64 + 1,
                    "{} segment {n}: gap or overlap",
                    f.subject
                );
                assert!(p.end >= p.begin);
                name = p.name;
                size = p.file_size;
                out.extend_from_slice(&p.data);
            }
            assert_eq!(
                out.len() as u64,
                size,
                "{name}: rebuilt size vs =ybegin size"
            );
            assert_eq!(
                out.len() as u64,
                f.bytes,
                "{name}: rebuilt size vs NZB bytes"
            );
            Decoded {
                yenc_name: name,
                data: out,
            }
        })
        .collect()
}

/// Asserts that the data files of a download are exactly `originals` (by content), returning the
/// decoded PAR2 files.
pub fn assert_same_files(decoded: &[Decoded], originals: &[(String, Vec<u8>)]) -> Vec<Decoded> {
    let mut want: BTreeMap<[u8; 16], Vec<&str>> = BTreeMap::new();
    for (n, b) in originals {
        if !b.is_empty() {
            want.entry(md5_of(b)).or_default().push(n);
        }
    }
    let mut got: BTreeMap<[u8; 16], Vec<&str>> = BTreeMap::new();
    let mut par2 = Vec::new();
    for d in decoded {
        if d.is_par2() {
            par2.push(d.clone());
        } else {
            got.entry(d.md5()).or_default().push(&d.yenc_name);
        }
    }
    let count = |m: &BTreeMap<[u8; 16], Vec<&str>>| -> BTreeMap<[u8; 16], usize> {
        m.iter().map(|(k, v)| (*k, v.len())).collect()
    };
    if count(&want) != count(&got) {
        let missing: Vec<_> = want.iter().filter(|(k, _)| !got.contains_key(*k)).collect();
        let extra: Vec<_> = got.iter().filter(|(k, _)| !want.contains_key(*k)).collect();
        panic!(
            "downloaded files differ from the originals: {} wanted, {} got; missing {missing:?}; unexpected {extra:?}",
            want.values().map(Vec::len).sum::<usize>(),
            got.values().map(Vec::len).sum::<usize>()
        );
    }
    par2
}

const PAR2_MAGIC: &[u8] = b"PAR2\0PKT";
const FILE_DESC: &[u8; 16] = b"PAR 2.0\0FileDesc";
const RECOVERY: &[u8; 16] = b"PAR 2.0\0RecvSlic";

/// A File Description packet: the name, length and MD5 a PAR2 set records for a data file.
#[derive(Debug, Clone)]
pub struct FileDesc {
    pub name: String,
    pub md5: [u8; 16],
    pub len: u64,
}

/// Lists the FileDesc packets in a PAR2 file and how many recovery slices it carries.
pub fn par2_contents(b: &[u8]) -> (Vec<FileDesc>, usize) {
    let mut descs = Vec::new();
    let mut recovery = 0;
    let mut i = 0;
    while i + 64 <= b.len() {
        assert_eq!(&b[i..i + 8], PAR2_MAGIC, "PAR2 packet magic at {i}");
        let len = u64::from_le_bytes(b[i + 8..i + 16].try_into().unwrap()) as usize;
        assert!(
            len >= 64 && i + len <= b.len(),
            "PAR2 packet length {len} at {i}"
        );
        let kind = &b[i + 48..i + 64];
        let body = &b[i + 64..i + len];
        if kind == FILE_DESC {
            let name = String::from_utf8_lossy(&body[56..])
                .trim_end_matches('\0')
                .to_string();
            descs.push(FileDesc {
                name,
                md5: body[16..32].try_into().unwrap(),
                len: u64::from_le_bytes(body[48..56].try_into().unwrap()),
            });
        } else if kind == RECOVERY {
            recovery += 1;
        }
        i += len;
    }
    (descs, recovery)
}

/// The data files a PAR2 set describes (deduplicated across its files).
pub fn par2_file_descs(par2: &[Decoded]) -> Vec<FileDesc> {
    let mut by_name: BTreeMap<String, FileDesc> = BTreeMap::new();
    for p in par2 {
        for d in par2_contents(&p.data).0 {
            by_name.insert(d.name.clone(), d);
        }
    }
    by_name.into_values().collect()
}

/// Writes a download to `dir` the way a newsreader does after a PAR2 rename: data files under the
/// names the PAR2 set records, PAR2 files as `set.par2` (the one without recovery slices) and
/// `set.volNN.par2`. Returns the PAR2 paths, main file first.
pub fn materialize(dir: &Path, decoded: &[Decoded]) -> Vec<PathBuf> {
    std::fs::create_dir_all(dir).unwrap();
    let par2: Vec<&Decoded> = decoded.iter().filter(|d| d.is_par2()).collect();
    assert!(!par2.is_empty(), "the download has no PAR2 files");
    let descs = par2_file_descs(&par2.iter().map(|d| (*d).clone()).collect::<Vec<_>>());
    let by_md5: HashMap<[u8; 16], &Decoded> = decoded
        .iter()
        .filter(|d| !d.is_par2())
        .map(|d| (d.md5(), d))
        .collect();
    for d in &descs {
        let data = by_md5.get(&d.md5).unwrap_or_else(|| {
            panic!(
                "PAR2 describes {} but no downloaded file matches it",
                d.name
            )
        });
        assert_eq!(data.data.len() as u64, d.len, "{}: PAR2 length", d.name);
        let path = dir.join(&d.name);
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p).unwrap();
        }
        std::fs::write(path, &data.data).unwrap();
    }
    let mut sorted: Vec<&Decoded> = par2;
    sorted.sort_by_key(|d| par2_contents(&d.data).1);
    let mut paths = Vec::new();
    for (i, d) in sorted.iter().enumerate() {
        let name = if i == 0 {
            "set.par2".to_string()
        } else {
            format!("set.vol{i:02}.par2")
        };
        let p = dir.join(name);
        std::fs::write(&p, &d.data).unwrap();
        paths.push(p);
    }
    paths
}

/// The `par2` command (par2cmdline), when installed. With `POSTIE_REQUIRE_PAR2` set (as in CI) a
/// missing binary fails the test instead of skipping the external check.
pub fn par2_cmd() -> Option<&'static str> {
    let found = Command::new("par2")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success());
    if found {
        return Some("par2");
    }
    assert!(
        std::env::var_os("POSTIE_REQUIRE_PAR2").is_none(),
        "POSTIE_REQUIRE_PAR2 is set but the `par2` command is not installed"
    );
    eprintln!("par2cmdline not installed: skipping the external PAR2 verify/repair check");
    None
}

/// Runs `par2 <op>` (verify or repair) on the set in `dir`; returns success and the combined output.
pub fn run_par2(bin: &str, op: &str, par2: &[PathBuf]) -> (bool, String) {
    let dir = par2[0].parent().unwrap();
    let out = Command::new(bin)
        .current_dir(dir)
        .arg(op)
        .arg("-q")
        .args(par2.iter().map(|p| p.file_name().unwrap()))
        .output()
        .expect("run par2");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (out.status.success(), text)
}
