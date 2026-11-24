//! Port of `internal/manifest`: durable, streaming, zstd-compressed JSON Lines manifests describing
//! every article of a source or PAR2 file in a transfer. They are written before network posting and
//! are the source of truth for verification, re-posting and crash recovery. The format is shared with
//! the Go app (a manifest written by either is read by both):
//!
//! ```text
//! {"v":1,"kind":"postie-transfer-manifest"}      // header line
//! {"i":0,"src":"/data/a.mkv","mid":"...",...}    // one ArticleRecord per line
//! ```
//!
//! Manifests are written to `<path>.tmp` and atomically renamed, so a crash never leaves a partial
//! manifest that looks complete.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, FixedOffset, Timelike};
use serde::{Deserialize, Serialize};

use crate::article::Article;

/// Current manifest format version; readers reject newer ones.
pub const VERSION: i64 = 1;
const KIND: &str = "postie-transfer-manifest";

pub const ROLE_ORIGINAL: &str = "original";
pub const ROLE_GENERATED_PAR2: &str = "generated_par2";
pub const ROLE_EXISTING_PAR2: &str = "existing_par2";

#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("creating manifest dir: {0}")]
    CreateDir(io::Error),
    #[error("creating manifest temp file: {0}")]
    CreateTemp(io::Error),
    #[error("writing manifest header: {0}")]
    WriteHeader(io::Error),
    #[error("encoding manifest record: {0}")]
    Encode(String),
    #[error("manifest writer already closed")]
    Closed,
    #[error("closing zstd writer: {0}")]
    CloseZstd(io::Error),
    #[error("syncing manifest: {0}")]
    Sync(io::Error),
    #[error("committing manifest: {0}")]
    Commit(io::Error),
    #[error("{0}")]
    Open(io::Error),
    #[error("reading manifest header: {0}")]
    ReadHeader(String),
    #[error("not a postie transfer manifest (kind={0:?})")]
    NotManifest(String),
    #[error("manifest version {0} is newer than supported {VERSION}")]
    TooNew(i64),
    #[error("{0}")]
    Io(#[from] io::Error),
}

#[derive(Serialize, Deserialize)]
struct Header {
    #[serde(rename = "v")]
    version: i64,
    kind: String,
}

/// Go marshals `time.Time` as RFC 3339 with nanoseconds (trailing zeros trimmed) and the zone offset.
mod go_time {
    use super::*;
    use serde::{Deserializer, Serializer};

    pub fn serialize<S: Serializer>(t: &DateTime<FixedOffset>, s: S) -> Result<S::Ok, S::Error> {
        let mut out = t.format("%Y-%m-%dT%H:%M:%S").to_string();
        let nanos = t.nanosecond() % 1_000_000_000;
        if nanos != 0 {
            out.push('.');
            out.push_str(format!("{nanos:09}").trim_end_matches('0'));
        }
        let off = t.offset().local_minus_utc();
        if off == 0 {
            out.push('Z');
        } else {
            let (sign, a) = if off < 0 { ('-', -off) } else { ('+', off) };
            out.push_str(&format!("{sign}{:02}:{:02}", a / 3600, (a % 3600) / 60));
        }
        s.serialize_str(&out)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<DateTime<FixedOffset>, D::Error> {
        let s = String::deserialize(d)?;
        DateTime::parse_from_rfc3339(&s).map_err(serde::de::Error::custom)
    }
}

fn is_empty_map(m: &BTreeMap<String, String>) -> bool {
    m.is_empty()
}

/// A single posted article (segment). Field tags are short to keep manifests with millions of
/// articles small.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArticleRecord {
    #[serde(rename = "i")]
    pub index: i64,
    #[serde(rename = "src")]
    pub source_path: String,
    #[serde(rename = "role")]
    pub file_role: String,
    #[serde(rename = "off")]
    pub offset: i64,
    #[serde(rename = "size")]
    pub body_size: u64,
    #[serde(rename = "mid")]
    pub message_id: String,
    #[serde(rename = "subj")]
    pub subject: String,
    #[serde(rename = "osubj", default, skip_serializing_if = "String::is_empty")]
    pub original_subject: String,
    pub from: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub groups: Vec<String>,
    #[serde(with = "go_time")]
    pub date: DateTime<FixedOffset>,
    #[serde(rename = "hdrs", default, skip_serializing_if = "is_empty_map")]
    pub custom_headers: BTreeMap<String, String>,
    #[serde(rename = "xnxg", default, skip_serializing_if = "String::is_empty")]
    pub xnxg_header: String,
    #[serde(rename = "fname")]
    pub file_name: String,
    #[serde(rename = "part")]
    pub part_number: i64,
    #[serde(rename = "parts")]
    pub total_parts: i64,
    #[serde(rename = "fsize")]
    pub file_size: i64,
}

impl ArticleRecord {
    /// Captures everything needed to re-post an article byte for byte with the same Message-ID.
    pub fn from_article(idx: i64, source_path: &str, role: &str, a: &Article) -> Self {
        Self {
            index: idx,
            source_path: source_path.into(),
            file_role: role.into(),
            offset: a.offset,
            body_size: a.size,
            message_id: a.message_id.clone(),
            subject: a.subject.clone(),
            original_subject: a.original_subject.clone(),
            from: a.from.clone(),
            groups: a.groups.clone(),
            date: a.date,
            custom_headers: a.custom_headers.clone(),
            xnxg_header: a.xnxg_header.clone(),
            file_name: a.file_name.clone(),
            part_number: a.part_number,
            total_parts: a.total_parts,
            file_size: a.file_size,
        }
    }
}

/// `<base_dir>/<transfer_id>/<file_id>.jsonl.zst`.
pub fn file_path(base_dir: &Path, transfer_id: &str, file_id: &str) -> PathBuf {
    base_dir
        .join(transfer_id)
        .join(format!("{file_id}.jsonl.zst"))
}

/// Go's `os.Remove`: a file, or an empty directory.
fn os_remove(p: &Path) -> io::Result<()> {
    match fs::remove_file(p) {
        Ok(()) => Ok(()),
        Err(file_err) => match fs::remove_dir(p) {
            Ok(()) => Ok(()),
            Err(dir_err) if p.is_dir() => Err(dir_err),
            Err(_) => Err(file_err),
        },
    }
}

/// Deletes a manifest and its per-transfer directory when that is empty. Never recursive: sibling
/// manifests or a writer's temporary file may still belong to a live upload.
pub fn remove(path: &Path, transfer_id: &str) -> io::Result<()> {
    if path.as_os_str().is_empty() {
        return Ok(());
    }
    if let Err(e) = os_remove(path)
        && e.kind() != io::ErrorKind::NotFound
    {
        return Err(e);
    }
    let Some(dir) = path.parent() else {
        return Ok(());
    };
    let base = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };
    if transfer_id.is_empty()
        || transfer_id == "."
        || transfer_id == ".."
        || base(dir) != transfer_id
        || !base(path).ends_with(".jsonl.zst")
    {
        return Ok(());
    }
    match os_remove(dir) {
        Ok(()) => Ok(()),
        Err(e)
            if matches!(
                e.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::DirectoryNotEmpty
                    | io::ErrorKind::AlreadyExists
            ) =>
        {
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Streams records to a temporary file; `commit` renames it into place, `abort` removes it.
pub struct Writer {
    final_path: PathBuf,
    tmp_path: PathBuf,
    enc: Option<zstd::stream::write::Encoder<'static, File>>,
    count: usize,
}

impl Writer {
    /// Creates the parent directory and a temporary file and writes the header.
    pub fn new(path: &Path) -> Result<Self, ManifestError> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(ManifestError::CreateDir)?;
        }
        let mut tmp = path.as_os_str().to_owned();
        tmp.push(".tmp");
        let tmp_path = PathBuf::from(tmp);
        let f = File::create(&tmp_path).map_err(ManifestError::CreateTemp)?;
        let enc = zstd::stream::write::Encoder::new(f, 0).map_err(ManifestError::CreateTemp)?;
        let mut w = Self {
            final_path: path.to_path_buf(),
            tmp_path,
            enc: Some(enc),
            count: 0,
        };
        let header = serde_json::to_string(&Header {
            version: VERSION,
            kind: KIND.into(),
        })
        .map_err(|e| ManifestError::Encode(e.to_string()))?;
        if let Err(e) = w.write_line(&header) {
            let _ = w.abort();
            return Err(ManifestError::WriteHeader(e));
        }
        Ok(w)
    }

    fn write_line(&mut self, line: &str) -> io::Result<()> {
        let enc = self
            .enc
            .as_mut()
            .ok_or_else(|| io::Error::other("closed"))?;
        enc.write_all(line.as_bytes())?;
        enc.write_all(b"\n")
    }

    pub fn write(&mut self, rec: &ArticleRecord) -> Result<(), ManifestError> {
        if self.enc.is_none() {
            return Err(ManifestError::Closed);
        }
        let line = serde_json::to_string(rec).map_err(|e| ManifestError::Encode(e.to_string()))?;
        self.write_line(&line)
            .map_err(|e| ManifestError::Encode(e.to_string()))?;
        self.count += 1;
        Ok(())
    }

    pub fn count(&self) -> usize {
        self.count
    }

    /// Flushes and fsyncs, then atomically renames into place.
    pub fn commit(&mut self) -> Result<(), ManifestError> {
        let enc = self.enc.take().ok_or(ManifestError::Closed)?;
        let f = match enc.finish() {
            Ok(f) => f,
            Err(e) => {
                let _ = fs::remove_file(&self.tmp_path);
                return Err(ManifestError::CloseZstd(e));
            }
        };
        if let Err(e) = f.sync_all() {
            let _ = fs::remove_file(&self.tmp_path);
            return Err(ManifestError::Sync(e));
        }
        drop(f);
        fs::rename(&self.tmp_path, &self.final_path).map_err(|e| {
            let _ = fs::remove_file(&self.tmp_path);
            ManifestError::Commit(e)
        })
    }

    /// Removes the temporary file without committing; a no-op after `commit`/`abort`.
    pub fn abort(&mut self) -> io::Result<()> {
        if self.enc.take().is_none() {
            return Ok(());
        }
        match fs::remove_file(&self.tmp_path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }
}

/// Streams records from a committed manifest; the header is validated on open.
pub struct Reader {
    lines: BufReader<zstd::stream::read::Decoder<'static, BufReader<File>>>,
    version: i64,
}

impl Reader {
    pub fn open(path: &Path) -> Result<Self, ManifestError> {
        let f = File::open(path).map_err(ManifestError::Open)?;
        let dec = zstd::stream::read::Decoder::new(f)
            .map_err(|e| ManifestError::ReadHeader(e.to_string()))?;
        let mut lines = BufReader::new(dec);
        let mut first = String::new();
        let n = lines
            .read_line(&mut first)
            .map_err(|e| ManifestError::ReadHeader(e.to_string()))?;
        if n == 0 {
            return Err(ManifestError::ReadHeader("EOF".into()));
        }
        let h: Header = serde_json::from_str(first.trim())
            .map_err(|e| ManifestError::ReadHeader(e.to_string()))?;
        if h.kind != KIND {
            return Err(ManifestError::NotManifest(h.kind));
        }
        if h.version > VERSION {
            return Err(ManifestError::TooNew(h.version));
        }
        Ok(Self {
            lines,
            version: h.version,
        })
    }

    pub fn version(&self) -> i64 {
        self.version
    }

    /// The next record, or `None` at the end.
    pub fn next_record(&mut self) -> Result<Option<ArticleRecord>, ManifestError> {
        loop {
            let mut line = String::new();
            if self.lines.read_line(&mut line)? == 0 {
                return Ok(None);
            }
            if line.trim().is_empty() {
                continue;
            }
            return serde_json::from_str(line.trim())
                .map(Some)
                .map_err(|e| ManifestError::Encode(e.to_string()));
        }
    }
}

impl Iterator for Reader {
    type Item = Result<ArticleRecord, ManifestError>;
    fn next(&mut self) -> Option<Self::Item> {
        self.next_record().transpose()
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "manifest-{}-{}",
                std::process::id(),
                rand::random::<u64>()
            ));
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn sample(n: usize) -> Vec<ArticleRecord> {
        (0..n)
            .map(|i| ArticleRecord {
                index: i as i64,
                source_path: "/data/show/ep1.mkv".into(),
                file_role: ROLE_ORIGINAL.into(),
                offset: i as i64 * 768_000,
                body_size: 768_000,
                message_id: format!("<part{i}@postie>"),
                subject: format!("subject line {i}"),
                original_subject: "ep1.mkv".into(),
                from: "Poster <poster@example.com>".into(),
                groups: vec!["alt.binaries.test".into(), "alt.binaries.misc".into()],
                date: FixedOffset::east_opt(0)
                    .unwrap()
                    .timestamp_opt(1_700_000_000 + i as i64, 0)
                    .unwrap(),
                custom_headers: BTreeMap::from([("X-Foo".into(), "bar".into())]),
                xnxg_header: "nxg-token".into(),
                file_name: "ep1.mkv".into(),
                part_number: i as i64 + 1,
                total_parts: n as i64,
                file_size: n as i64 * 768_000,
            })
            .collect()
    }

    #[test]
    fn write_read_round_trip() {
        let t = Tmp::new();
        let path = t.0.join("t1").join("file1.jsonl.zst");
        let want = sample(5);
        let mut w = Writer::new(&path).unwrap();
        for r in &want {
            w.write(r).unwrap();
        }
        assert_eq!(w.count(), 5);
        w.commit().unwrap();
        let mut r = Reader::open(&path).unwrap();
        assert_eq!(r.version(), VERSION);
        let got: Vec<_> = r.by_ref().map(Result::unwrap).collect();
        assert_eq!(got, want);
        assert!(matches!(w.write(&want[0]), Err(ManifestError::Closed)));
    }

    #[test]
    fn commit_is_atomic_and_abort_cleans_up() {
        let t = Tmp::new();
        let path = t.0.join("t1").join("file1.jsonl.zst");
        let mut w = Writer::new(&path).unwrap();
        w.write(&sample(1)[0]).unwrap();
        assert!(!path.exists() && PathBuf::from(format!("{}.tmp", path.display())).exists());
        w.commit().unwrap();
        assert!(path.exists() && !PathBuf::from(format!("{}.tmp", path.display())).exists());

        let p2 = t.0.join("t1").join("file2.jsonl.zst");
        let mut w = Writer::new(&p2).unwrap();
        w.write(&sample(1)[0]).unwrap();
        w.abort().unwrap();
        assert!(!p2.exists() && !PathBuf::from(format!("{}.tmp", p2.display())).exists());
        w.abort().unwrap();
    }

    #[test]
    fn open_rejects_non_manifest_and_newer_versions() {
        let t = Tmp::new();
        let bogus = t.0.join("bogus.jsonl.zst");
        fs::write(&bogus, "not a manifest").unwrap();
        assert!(Reader::open(&bogus).is_err());
        let newer = t.0.join("newer.jsonl.zst");
        let mut enc = zstd::stream::write::Encoder::new(File::create(&newer).unwrap(), 0).unwrap();
        enc.write_all(b"{\"v\":2,\"kind\":\"postie-transfer-manifest\"}\n")
            .unwrap();
        enc.finish().unwrap();
        assert!(matches!(
            Reader::open(&newer),
            Err(ManifestError::TooNew(2))
        ));
        let other = t.0.join("other.jsonl.zst");
        let mut enc = zstd::stream::write::Encoder::new(File::create(&other).unwrap(), 0).unwrap();
        enc.write_all(b"{\"v\":1,\"kind\":\"other\"}\n").unwrap();
        enc.finish().unwrap();
        assert_eq!(
            Reader::open(&other).err().unwrap().to_string(),
            "not a postie transfer manifest (kind=\"other\")"
        );
    }

    /// A manifest written by the Go app (klauspost zstd + encoding/json) is read unchanged.
    #[test]
    fn reads_a_manifest_written_by_go() {
        let path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/go-manifest.jsonl.zst");
        let recs: Vec<_> = Reader::open(&path).unwrap().map(Result::unwrap).collect();
        assert_eq!(recs.len(), 2);
        let a = &recs[0];
        assert_eq!(
            (
                a.index,
                a.message_id.as_str(),
                a.file_role.as_str(),
                a.body_size,
                a.part_number
            ),
            (0, "abc@postie", ROLE_ORIGINAL, 768_000, 1)
        );
        assert_eq!(a.subject, "[1/1] \"a.mkv\" - yEnc (1/2) <x&y>");
        assert_eq!(a.from, "Poster <p@example.com>");
        assert_eq!(a.groups, ["alt.binaries.test", "alt.binaries.misc"]);
        assert_eq!(
            a.date.to_rfc3339_opts(chrono::SecondsFormat::Nanos, false),
            "2023-11-14T23:13:20.123456789+01:00"
        );
        assert_eq!(
            a.custom_headers.get("X-Foo").map(String::as_str),
            Some("bar")
        );
        let b = &recs[1];
        assert_eq!(
            (
                b.file_role.as_str(),
                b.file_name.as_str(),
                b.groups.len(),
                b.custom_headers.len()
            ),
            (ROLE_GENERATED_PAR2, "日本.mkv", 0, 0)
        );
        assert_eq!(b.date.offset().local_minus_utc(), 0);
    }

    #[test]
    fn serialized_records_use_go_field_names_and_omit_empties() {
        let mut r = sample(1).remove(0);
        r.original_subject.clear();
        r.groups.clear();
        r.custom_headers.clear();
        r.xnxg_header.clear();
        let v: serde_json::Value = serde_json::to_value(&r).unwrap();
        let obj = v.as_object().unwrap();
        for k in [
            "i", "src", "role", "off", "size", "mid", "subj", "from", "date", "fname", "part",
            "parts", "fsize",
        ] {
            assert!(obj.contains_key(k), "missing {k}");
        }
        for k in ["osubj", "groups", "hdrs", "xnxg"] {
            assert!(!obj.contains_key(k), "{k} should be omitted");
        }
        assert_eq!(obj["date"], "2023-11-14T22:13:20Z");
    }

    #[test]
    fn record_from_article_and_file_path() {
        let mut a = Article::new(
            "id@h",
            "s",
            "os",
            "f",
            vec!["g".into()],
            2,
            3,
            100,
            "f.bin",
            1,
            "f.bin",
            BTreeMap::new(),
        );
        a.offset = 50;
        a.size = 25;
        let r = ArticleRecord::from_article(1, "/src", ROLE_EXISTING_PAR2, &a);
        assert_eq!(
            (
                r.index,
                r.offset,
                r.body_size,
                r.part_number,
                r.total_parts,
                r.message_id.as_str()
            ),
            (1, 50, 25, 2, 3, "id@h")
        );
        assert_eq!(
            file_path(Path::new("/base"), "t", "f"),
            PathBuf::from("/base/t/f.jsonl.zst")
        );
    }

    /// Writes a manifest to `$MANIFEST_OUT` for the manual check that the Go reader accepts it.
    #[test]
    #[ignore = "writes a file for the manual Go cross-check"]
    fn write_manifest_for_go() {
        let mut w = Writer::new(Path::new(&std::env::var("MANIFEST_OUT").unwrap())).unwrap();
        for r in sample(3) {
            w.write(&r).unwrap();
        }
        w.commit().unwrap();
    }

    #[test]
    fn large_manifest_streams() {
        let t = Tmp::new();
        let path = t.0.join("big.jsonl.zst");
        let mut w = Writer::new(&path).unwrap();
        let rec = sample(1).remove(0);
        for i in 0..20_000 {
            w.write(&ArticleRecord {
                index: i,
                ..rec.clone()
            })
            .unwrap();
        }
        w.commit().unwrap();
        let mut n = 0;
        for (i, r) in Reader::open(&path).unwrap().enumerate() {
            assert_eq!(r.unwrap().index, i as i64);
            n += 1;
        }
        assert_eq!(n, 20_000);
    }

    #[test]
    fn remove_preserves_live_files_and_unrelated_directories() {
        // (name, transfer_id, parent, filename, sibling, want_dir)
        let cases = [
            (
                "empty transfer",
                "transfer",
                "transfer",
                "f.jsonl.zst",
                false,
                false,
            ),
            (
                "live sibling",
                "transfer",
                "transfer",
                "f.jsonl.zst",
                true,
                true,
            ),
            (
                "unrelated parent",
                "transfer",
                "other",
                "f.jsonl.zst",
                false,
                true,
            ),
            ("empty id", "", "transfer", "f.jsonl.zst", false, true),
            (
                "nonstandard file",
                "transfer",
                "transfer",
                "manifest",
                false,
                true,
            ),
        ];
        for (name, tid, parent, filename, sibling, want_dir) in cases {
            let t = Tmp::new();
            let dir = t.0.join(parent);
            fs::create_dir(&dir).unwrap();
            let path = dir.join(filename);
            fs::write(&path, "manifest").unwrap();
            let sib = dir.join("live.jsonl.zst.tmp");
            if sibling {
                fs::write(&sib, "in progress").unwrap();
            }
            for _ in 0..2 {
                remove(&path, tid).unwrap_or_else(|e| panic!("{name}: {e}"));
            }
            assert!(!path.exists(), "{name}");
            assert_eq!(dir.exists(), want_dir, "{name}");
            assert!(t.0.exists());
            if sibling {
                assert_eq!(fs::read_to_string(&sib).unwrap(), "in progress");
            }
        }
    }

    #[test]
    fn remove_failure_retains_directory_contents_and_empty_path_is_a_noop() {
        let t = Tmp::new();
        let path = file_path(&t.0, "transfer", "f");
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("recovery-data"), "retain me").unwrap();
        assert!(remove(&path, "transfer").is_err());
        assert_eq!(
            fs::read_to_string(path.join("recovery-data")).unwrap(),
            "retain me"
        );
        assert!(t.0.join("transfer").exists());
        remove(Path::new(""), "transfer").unwrap();
    }
}
