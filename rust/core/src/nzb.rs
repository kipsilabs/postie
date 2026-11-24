//! Port of `internal/nzb`: the NZB generator (and `SaveExisting` for provider NZBs that are reused
//! through the duplicate lookup). The XML is written by hand in the exact layout of
//! `nzbparser.Write` (`xml.MarshalIndent`, two-space indent). Two deliberate differences from Go
//! (ADR-014): segment ids and meta values are XML-escaped (Go writes them raw), and `<meta>`
//! elements are sorted by type instead of random map order. Compression levels are real codec
//! levels (zstd 1-22), which also fixes Go's zstd levels above 4 failing at write time.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{Local, SecondsFormat};
use tokio_util::sync::CancellationToken;

use crate::article::Article;
use crate::config::NzbCompressionConfig;

const HEADER: &str = "<?xml version=\"1.0\" encoding=\"utf-8\" ?>\n<!DOCTYPE nzb PUBLIC \"-//newzBin//DTD NZB 1.1//EN\" \"http://www.newzbin.com/DTD/nzb/nzb-1.1.dtd\">\n";
const XMLNS: &str = "http://www.newzbin.com/DTD/2003/nzb";

/// Go's `xml.EscapeText`: used for attribute values and group names.
fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&#34;"),
            '\'' => out.push_str("&#39;"),
            '\t' => out.push_str("&#x9;"),
            '\n' => out.push_str("&#xA;"),
            '\r' => out.push_str("&#xD;"),
            c if matches!(c, '\u{20}'..='\u{D7FF}' | '\u{E000}'..='\u{FFFD}' | '\u{10000}'..='\u{10FFFF}') => {
                out.push(c)
            }
            _ => out.push('\u{FFFD}'),
        }
    }
    out
}

/// Minimal escaping for the text that Go writes raw (segment ids, meta values).
fn esc_min(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[derive(Default)]
struct State {
    /// filename -> articles
    articles: BTreeMap<String, Vec<Article>>,
    /// filename -> message id -> index (O(1) replace)
    index: HashMap<String, HashMap<String, usize>>,
    hashes: HashMap<String, String>,
}

/// Collects articles and writes the NZB.
pub struct Generator {
    segment_size: u64,
    compression: NzbCompressionConfig,
    maintain_original_extension: bool,
    state: Mutex<State>,
}

/// `foo.ext` becomes `foo.ext.nzb` (maintain) or `foo.nzb`; a path already ending in `.nzb` is kept.
pub fn final_nzb_path(original: &Path, maintain_original_extension: bool) -> PathBuf {
    let s = original.to_string_lossy();
    if s.to_lowercase().ends_with(".nzb") {
        return original.to_path_buf();
    }
    let dir = original.parent().unwrap_or(Path::new(""));
    let base = original
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = if maintain_original_extension {
        format!("{base}.nzb")
    } else {
        let stem = base.rfind('.').map_or(base.as_str(), |i| &base[..i]); // Go's filepath.Ext
        format!("{stem}.nzb")
    };
    dir.join(name)
}

fn compress_to(
    path: &Path,
    kind: &str,
    level: i64,
    data: &[u8],
    entry_name: &str,
) -> io::Result<()> {
    let f = File::create(path)?;
    match kind {
        "zstd" => {
            let level = if level == 0 { 3 } else { level.clamp(1, 22) } as i32;
            let mut enc = zstd::stream::write::Encoder::new(f, level)?;
            enc.write_all(data)?;
            enc.finish()?;
        }
        "brotli" => {
            let mut w = brotli::CompressorWriter::new(f, 4096, level.clamp(0, 11) as u32, 22);
            w.write_all(data)?;
            w.flush()?;
            drop(w.into_inner());
        }
        _ => {
            let mut z = zip::ZipWriter::new(f);
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated)
                .compression_level(Some(level.clamp(0, 9)));
            z.start_file(entry_name, opts).map_err(io::Error::other)?;
            z.write_all(data)?;
            z.finish().map_err(io::Error::other)?;
        }
    }
    Ok(())
}

impl Generator {
    pub fn new(
        segment_size: u64,
        compression: NzbCompressionConfig,
        maintain_original_extension: bool,
    ) -> Self {
        Self {
            segment_size,
            compression,
            maintain_original_extension,
            state: Mutex::new(State::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Adds an article, replacing one with the same message id for the same file.
    pub fn add_article(&self, art: Article) {
        let mut s = self.lock();
        let file = art.original_name.clone();
        if let Some(&i) = s.index.get(&file).and_then(|m| m.get(&art.message_id)) {
            s.articles
                .get_mut(&file)
                .expect("indexed file has articles")[i] = art;
            return;
        }
        let list = s.articles.entry(file.clone()).or_default();
        let pos = list.len();
        let id = art.message_id.clone();
        list.push(art);
        s.index.entry(file).or_default().insert(id, pos);
    }

    /// Drops every article and hash recorded for `filename` (an abandoned upload). Unknown names are a no-op.
    pub fn remove_file(&self, filename: &str) {
        let mut s = self.lock();
        s.articles.remove(filename);
        s.index.remove(filename);
        s.hashes.remove(filename);
    }

    pub fn add_file_hash(&self, filename: &str, hash: &str) {
        self.lock()
            .hashes
            .insert(filename.to_string(), hash.to_string());
    }

    /// The NZB XML for everything collected so far.
    pub fn render(&self) -> Result<String, String> {
        let mut s = self.lock();
        if s.articles.is_empty() {
            return Err("no articles found".into());
        }
        let now = Local::now();
        let hashes = s.hashes.clone();
        let mut files: Vec<(i64, String)> = Vec::new();
        for (filename, arts) in &mut s.articles {
            if arts.is_empty() {
                continue;
            }
            arts.sort_by_key(|a| a.part_number); // stable: ties keep insertion order
            let size: u64 = arts.iter().map(|a| a.size).sum();
            let first = &arts[0];
            let mut x = format!(
                "  <file poster=\"{}\" date=\"{}\" subject=\"{}\" bytes=\"{}\" filehash=\"{}\">\n",
                esc(&first.from),
                now.timestamp(),
                esc(&first.original_subject),
                size,
                esc(hashes.get(filename).map_or("", String::as_str))
            );
            if !first.groups.is_empty() {
                x.push_str("    <groups>\n");
                for g in &first.groups {
                    x.push_str(&format!("      <group>{}</group>\n", esc(g)));
                }
                x.push_str("    </groups>\n");
            }
            x.push_str("    <segments>\n");
            for (i, a) in arts.iter().enumerate() {
                // the configured segment size for every segment except the last
                let bytes = if i == arts.len() - 1 {
                    a.size
                } else {
                    self.segment_size
                };
                x.push_str(&format!(
                    "      <segment bytes=\"{}\" number=\"{}\">{}</segment>\n",
                    bytes,
                    a.part_number,
                    esc_min(&a.message_id)
                ));
            }
            x.push_str("    </segments>\n  </file>\n");
            files.push((first.file_number, x));
        }
        files.sort_by_key(|f| f.0); // stable: equal numbers keep filename order
        let mut out = String::from(HEADER);
        out.push_str(&format!("<nzb xmlns=\"{XMLNS}\">\n  <head>\n"));
        let meta = [
            ("chunk_size", self.segment_size.to_string()),
            ("date", now.to_rfc3339_opts(SecondsFormat::Secs, true)),
        ];
        for (k, v) in meta {
            out.push_str(&format!("    <meta type=\"{k}\">{}</meta>\n", esc_min(&v)));
        }
        out.push_str("  </head>\n");
        for (_, x) in files {
            out.push_str(&x);
        }
        out.push_str("</nzb>");
        Ok(out)
    }

    /// Writes the NZB (optionally compressed) and returns its final path.
    pub fn generate(&self, output_path: &Path) -> Result<PathBuf, String> {
        let xml = self.render()?;
        let final_path = final_nzb_path(output_path, self.maintain_original_extension);
        let dir = output_path.parent().unwrap_or(Path::new(""));
        if !dir.as_os_str().is_empty() {
            match fs::metadata(dir) {
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {
                    fs::create_dir_all(dir).map_err(|e| {
                        format!("failed to create output directory: {e}, {}", dir.display())
                    })?;
                }
                Err(e) => {
                    return Err(format!(
                        "failed to check output directory: {e}, {}",
                        dir.display()
                    ));
                }
            }
        }
        let c = &self.compression;
        let (suffix, kind) = match (c.enabled, c.kind.as_str()) {
            (true, "zstd") => (".zst", "zstd"),
            (true, "brotli") => (".br", "brotli"),
            (true, "zip") => (".zip", "zip"),
            _ => ("", ""),
        };
        if kind.is_empty() {
            fs::write(&final_path, xml.as_bytes())
                .map_err(|e| format!("error writing NZB file: {e}"))?;
            return Ok(final_path);
        }
        let mut name = final_path.clone().into_os_string();
        name.push(suffix);
        let target = PathBuf::from(name);
        let entry = final_path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        compress_to(&target, kind, c.level, xml.as_bytes(), &entry)
            .map_err(|e| format!("error compressing NZB file with {kind}: {e}"))?;
        Ok(target)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SaveError {
    #[error("{0}")]
    Invalid(String),
    #[error("context canceled")]
    Canceled,
    #[error("publish reused NZB: {0}")]
    Publish(io::Error),
    #[error("{0}")]
    Io(#[from] io::Error),
}

/// Publishes a validated provider NZB atomically with the normal naming and compression. It never
/// synthesizes article records or manifests.
pub fn save_existing(
    cancel: &CancellationToken,
    data: &[u8],
    target: &Path,
    cfg: &NzbCompressionConfig,
    maintain_extension: bool,
) -> Result<PathBuf, SaveError> {
    duplicatelookup_rs::port::validate_nzb_bytes(data).map_err(SaveError::Invalid)?;
    if cancel.is_cancelled() {
        return Err(SaveError::Canceled);
    }
    let uncompressed = final_nzb_path(target, maintain_extension);
    let (suffix, kind) = match (cfg.enabled, cfg.kind.as_str()) {
        (true, "zstd") => (".zst", "zstd"),
        (true, "brotli") => (".br", "brotli"),
        (true, "zip") => (".zip", "zip"),
        _ => ("", ""),
    };
    let mut t = uncompressed.clone().into_os_string();
    t.push(suffix);
    let target = PathBuf::from(t);
    let dir = target.parent().unwrap_or(Path::new("")).to_path_buf();
    if !dir.as_os_str().is_empty() {
        fs::create_dir_all(&dir)?;
    }
    let tmp = dir.join(format!(".duplicate-nzb-{:016x}", rand::random::<u64>()));
    let result = (|| -> Result<(), SaveError> {
        let name = uncompressed
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if kind.is_empty() {
            let mut f = File::create(&tmp)?;
            f.write_all(data)?;
            f.sync_all()?;
        } else {
            compress_to(&tmp, kind, cfg.level, data, &name)?;
            fs::OpenOptions::new().write(true).open(&tmp)?.sync_all()?;
        }
        if cancel.is_cancelled() {
            return Err(SaveError::Canceled);
        }
        fs::rename(&tmp, &target).map_err(SaveError::Publish)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map(|()| target)
}

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!(
                "nzb-{}-{}",
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

    fn art(id: &str, part: i64, size: u64, name: &str, file_no: i64, subject: &str) -> Article {
        let mut a = Article::new(
            id,
            "",
            subject,
            "Poster <p@example.com>",
            vec!["alt.test".into(), "alt.binaries.\"x\"".into()],
            part,
            3,
            0,
            name,
            file_no,
            name,
            BTreeMap::new(),
        );
        a.size = size;
        a
    }

    fn compression_level(kind: &str, level: i64) -> NzbCompressionConfig {
        NzbCompressionConfig {
            enabled: !kind.is_empty() && kind != "none",
            kind: kind.into(),
            level,
        }
    }

    fn compression(kind: &str) -> NzbCompressionConfig {
        compression_level(kind, 3)
    }

    /// The set the Go generator wrote to `tests/fixtures/set.nzb`.
    fn fixture_set(g: &Generator) {
        let s1 = "[1/2] \"a.bin\" & <yEnc> 'x'";
        g.add_article(art("b@h", 2, 1000, "a.bin", 1, s1));
        g.add_article(art("a@h", 1, 1000, "a.bin", 1, s1));
        g.add_article(art("c@h", 3, 250, "a.bin", 1, s1));
        g.add_article(art("z@h", 1, 700, "b.bin", 2, "[2/2] \"b.bin\""));
        g.add_hash();
    }
    impl Generator {
        fn add_hash(&self) {
            self.add_file_hash("a.bin", "abcdef");
        }
    }

    /// Masks the two timestamps and sorts the `<meta>` lines (Go's order is random).
    fn normalize(xml: &str) -> String {
        let mut lines: Vec<String> = xml
            .lines()
            .map(|l| {
                let mut l = l.to_string();
                if let Some(i) = l.find(" date=\"") {
                    let j = l[i + 7..].find('"').unwrap() + i + 7;
                    l.replace_range(i + 7..j, "TS");
                }
                if l.contains("<meta type=\"date\">") {
                    l = "    <meta type=\"date\">TS</meta>".into();
                }
                l
            })
            .collect();
        let (a, b) = (
            lines.iter().position(|l| l.contains("<meta")).unwrap(),
            lines.iter().rposition(|l| l.contains("<meta")).unwrap(),
        );
        lines[a..=b].sort();
        lines.join("\n")
    }

    #[test]
    fn output_matches_go_nzbparser_write() {
        let g = Generator::new(1000, compression(""), true);
        fixture_set(&g);
        let go = fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/set.nzb"
        ))
        .unwrap();
        assert_eq!(normalize(&g.render().unwrap()), normalize(&go));
    }

    /// Writes the fixture set to `$NZB_OUT` for the manual check that Go's parser accepts it.
    #[test]
    #[ignore = "writes a file for the manual Go cross-check"]
    fn write_nzb_for_go() {
        let g = Generator::new(1000, compression(""), true);
        fixture_set(&g);
        g.generate(Path::new(&std::env::var("NZB_OUT").unwrap()))
            .unwrap();
    }

    #[test]
    fn add_article_replaces_by_message_id_and_sorts_by_part() {
        let g = Generator::new(1000, compression(""), true);
        g.add_article(art("id1", 1, 500, "f.txt", 1, "S"));
        g.add_article(art("id1", 1, 600, "f.txt", 1, "Updated"));
        g.add_article(art("id2", 2, 500, "f.txt", 1, "S"));
        let xml = g.render().unwrap();
        assert!(
            xml.contains("subject=\"Updated\"") && xml.contains("bytes=\"1100\""),
            "{xml}"
        );
        assert_eq!(xml.matches("<segment ").count(), 2);
    }

    #[test]
    fn segments_use_the_configured_size_except_the_last() {
        let g = Generator::new(1000, compression(""), true);
        g.add_article(art("a", 1, 400, "f", 1, "S"));
        g.add_article(art("b", 2, 300, "f", 1, "S"));
        let xml = g.render().unwrap();
        assert!(
            xml.contains("<segment bytes=\"1000\" number=\"1\">a</segment>")
                && xml.contains("<segment bytes=\"300\" number=\"2\">b</segment>")
        );
    }

    #[test]
    fn ids_and_meta_are_escaped_deliberately() {
        let g = Generator::new(1000, compression(""), true);
        g.add_article(art("a&b<c>@h", 1, 5, "f", 1, "S"));
        assert!(
            g.render()
                .unwrap()
                .contains("<segment bytes=\"5\" number=\"1\">a&amp;b&lt;c&gt;@h</segment>")
        );
    }

    #[test]
    fn generate_without_articles_fails() {
        let t = Tmp::new();
        let g = Generator::new(1000, compression(""), true);
        assert_eq!(
            g.generate(&t.0.join("x.nzb")).unwrap_err(),
            "no articles found"
        );
    }

    #[test]
    fn remove_file_drops_articles_and_hash() {
        let g = Generator::new(1000, compression(""), true);
        g.add_article(art("a", 1, 5, "keep", 1, "S"));
        g.add_article(art("b", 1, 5, "drop", 2, "S"));
        g.add_file_hash("drop", "h");
        g.remove_file("drop");
        g.remove_file("never-existed");
        let xml = g.render().unwrap();
        assert!(!xml.contains(">b</segment>") && xml.contains(">a</segment>"));
    }

    #[test]
    fn path_naming_matches_go() {
        let n = |p: &str, keep| {
            crate::testutil::slash(
                final_nzb_path(Path::new(p), keep)
                    .to_string_lossy()
                    .as_ref(),
            )
        };
        assert_eq!(n("/o/Sun.Moments.nzb", false), "/o/Sun.Moments.nzb");
        assert_eq!(n("/o/UP.NZB", true), "/o/UP.NZB");
        assert_eq!(n("/o/movie.mkv", true), "/o/movie.mkv.nzb");
        assert_eq!(n("/o/movie.mkv", false), "/o/movie.nzb");
        assert_eq!(n("/o/a.b.rar", false), "/o/a.b.nzb");
        assert_eq!(n("/o/noext", false), "/o/noext.nzb");
        assert_eq!(n("/o/.hidden", false), "/o/.nzb");
    }

    fn read_back(path: &Path, kind: &str) -> Vec<u8> {
        let raw = fs::read(path).unwrap();
        match kind {
            "zstd" => zstd::decode_all(&raw[..]).unwrap(),
            "brotli" => {
                let mut out = Vec::new();
                brotli::Decompressor::new(&raw[..], 4096)
                    .read_to_end(&mut out)
                    .unwrap();
                out
            }
            "zip" => {
                let mut z = zip::ZipArchive::new(File::open(path).unwrap()).unwrap();
                assert_eq!(z.len(), 1);
                let mut out = Vec::new();
                z.by_index(0).unwrap().read_to_end(&mut out).unwrap();
                out
            }
            _ => raw,
        }
    }

    #[test]
    fn generate_with_each_compression_round_trips() {
        for (kind, level, suffix) in [
            ("", 0, ""),
            ("zstd", 3, ".zst"),
            ("zstd", 19, ".zst"),
            ("brotli", 4, ".br"),
            ("zip", 6, ".zip"),
        ] {
            let t = Tmp::new();
            let g = Generator::new(1000, compression_level(kind, level), true);
            g.add_article(art("a", 1, 5, "f.bin", 1, "S"));
            let p = g.generate(&t.0.join("out").join("f.bin")).unwrap();
            assert_eq!(
                p.file_name().unwrap().to_string_lossy(),
                format!("f.bin.nzb{suffix}")
            );
            let xml = String::from_utf8(read_back(&p, kind)).unwrap();
            assert!(
                xml.starts_with("<?xml") && xml.ends_with("</nzb>"),
                "{kind}"
            );
        }
    }

    const EXISTING: &str = r#"<nzb><file subject='"movie.mkv" yEnc (1/1)'><groups><group>alt.test</group></groups><segments><segment bytes="3" number="1">existing@test</segment></segments></file></nzb>"#;

    #[test]
    fn save_existing_naming_compression_and_cancellation() {
        for kind in ["none", "zstd", "brotli", "zip"] {
            for keep in [false, true] {
                let t = Tmp::new();
                let target = t.0.join("nested").join("movie.mkv");
                let cfg = compression_level(kind, 3);
                let p = save_existing(
                    &CancellationToken::new(),
                    EXISTING.as_bytes(),
                    &target,
                    &cfg,
                    keep,
                )
                .unwrap();
                let suffix = match kind {
                    "zstd" => ".zst",
                    "brotli" => ".br",
                    "zip" => ".zip",
                    _ => "",
                };
                let name = if keep { "movie.mkv.nzb" } else { "movie.nzb" };
                assert_eq!(
                    p.file_name().unwrap().to_string_lossy(),
                    format!("{name}{suffix}")
                );
                assert_eq!(read_back(&p, kind), EXISTING.as_bytes(), "{kind}/{keep}");
                let leaked = fs::read_dir(p.parent().unwrap())
                    .unwrap()
                    .filter_map(Result::ok)
                    .filter(|e| {
                        e.file_name()
                            .to_string_lossy()
                            .starts_with(".duplicate-nzb-")
                    })
                    .count();
                assert_eq!(leaked, 0);
                let cancelled = CancellationToken::new();
                cancelled.cancel();
                assert!(matches!(
                    save_existing(
                        &cancelled,
                        EXISTING.as_bytes(),
                        &t.0.join("movie.mkv-cancel"),
                        &cfg,
                        keep
                    ),
                    Err(SaveError::Canceled)
                ));
            }
        }
        let t = Tmp::new();
        assert!(
            matches!(save_existing(&CancellationToken::new(), b"<nzb/>", &t.0.join("x"), &compression(""), true), Err(SaveError::Invalid(m)) if m == "lookup NZB contains no files")
        );
    }
}
