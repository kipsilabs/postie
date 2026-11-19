//! File intake for the one-shot upload (ports `collectDirectoryFiles`).

use std::path::Path;

use postie_core::fileinfo::FileInfo;
use postie_core::watcher::globmatch::filepath_match;

pub fn single(path: &str) -> Result<(Vec<FileInfo>, String), String> {
    let meta = std::fs::metadata(path).map_err(|e| format!("stat {path}: {e}"))?;
    let dir = Path::new(path)
        .parent()
        .map_or_else(|| ".".into(), |p| p.to_string_lossy().into_owned());
    let dir = if dir.is_empty() { ".".into() } else { dir };
    Ok((
        vec![FileInfo {
            path: path.into(),
            size: meta.len(),
            ..FileInfo::default()
        }],
        dir,
    ))
}

pub fn collect_directory(dir: &str, patterns: &[String]) -> Result<Vec<FileInfo>, String> {
    for p in patterns {
        // star chunks removed so the whole pattern is validated even when a prefix doesn't match
        if filepath_match(&p.replace('*', "?"), "").is_err() {
            return Err(format!(
                "invalid ignore pattern {p:?}: syntax error in pattern"
            ));
        }
    }
    let mut out = Vec::new();
    walk(Path::new(dir), patterns, &mut out)?;
    Ok(out)
}

fn walk(dir: &Path, patterns: &[String], out: &mut Vec<FileInfo>) -> Result<(), String> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("open {}: {e}", dir.display()))?
        .collect::<Result<_, _>>()
        .map_err(|e| e.to_string())?;
    // filepath.Walk visits entries in lexical order
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let path = e.path();
        let meta = std::fs::metadata(&path).map_err(|e| e.to_string())?;
        if meta.is_dir() {
            walk(&path, patterns, out)?;
            continue;
        }
        let name = e.file_name().to_string_lossy().into_owned();
        if patterns
            .iter()
            .any(|p| filepath_match(p, &name).unwrap_or(false))
        {
            continue;
        }
        out.push(FileInfo {
            path: path.to_string_lossy().into_owned(),
            size: meta.len(),
            ..FileInfo::default()
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn walks_sorted_and_ignores_by_basename() {
        let d = std::env::temp_dir().join(format!("postie-cli-{}", std::process::id()));
        std::fs::create_dir_all(d.join("sub")).unwrap();
        for f in ["b.mkv", "a.mkv", "x.tmp", "sub/c.mkv"] {
            std::fs::write(d.join(f), b"12").unwrap();
        }
        let got = collect_directory(d.to_str().unwrap(), &["*.tmp".into()]).unwrap();
        let names: Vec<_> = got
            .iter()
            .map(|f| {
                Path::new(&f.path)
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, ["a.mkv", "b.mkv", "c.mkv"]);
        assert!(collect_directory(d.to_str().unwrap(), &["[".into()]).is_err());
        std::fs::remove_dir_all(&d).ok();
    }
}
