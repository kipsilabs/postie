//! Path helpers of `pkg/postie`: Go's `filepath.Rel` semantics plus the folder/relative-path rules.

use std::path::{Component, Path};

use crate::fileinfo::FileInfo;

fn clean_parts(p: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in Path::new(p).components() {
        match c {
            Component::CurDir | Component::RootDir | Component::Prefix(_) => {}
            Component::ParentDir => {
                if out.last().is_some_and(|l| l != "..") {
                    out.pop();
                } else if !p.starts_with('/') {
                    out.push("..".into());
                }
            }
            Component::Normal(s) => out.push(s.to_string_lossy().into_owned()),
        }
    }
    out
}

/// Go's `filepath.Rel(base, target)`: `.` when equal, `../x` for siblings, `None` when one path is
/// absolute and the other is not or `base` climbs above the common root.
pub fn filepath_rel(base: &str, target: &str) -> Option<String> {
    if Path::new(base).is_absolute() != Path::new(target).is_absolute() {
        return None;
    }
    let (b, t) = (clean_parts(base), clean_parts(target));
    let common = b.iter().zip(&t).take_while(|(x, y)| x == y).count();
    if b[common..].iter().any(|s| s == "..") {
        return None;
    }
    let mut parts: Vec<&str> = vec![".."; b.len() - common];
    parts.extend(t[common..].iter().map(String::as_str));
    Some(if parts.is_empty() {
        ".".into()
    } else {
        parts.join("/")
    })
}

/// Directory of `file_path` relative to `root_dir`; empty when equal or not expressible (output then
/// goes straight into the output dir).
pub fn relative_path_from(root_dir: &str, file_path: &str) -> String {
    let dir = Path::new(file_path)
        .parent()
        .map(|d| d.to_string_lossy().into_owned())
        .unwrap_or_default();
    match filepath_rel(root_dir, &dir) {
        Some(r) if r != "." => r,
        _ => String::new(),
    }
}

/// The top-level subfolder name of the files under `root_dir`, falling back to `root_dir`'s own name
/// and finally `upload`.
pub fn derive_folder_name(root_dir: &str, files: &[FileInfo]) -> String {
    if let Some(first) = files.first()
        && let Some(rel) = filepath_rel(root_dir, &first.path)
        && let Some((head, _)) = rel.split_once('/')
        && head != "."
        && !head.is_empty()
    {
        return head.to_string();
    }
    match Path::new(root_dir)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
    {
        Some(n) if n != "." && !n.is_empty() => n,
        _ => "upload".into(),
    }
}

/// `paths` minus anything already in `source_paths` (and duplicates): reused source PAR2 files keep
/// their relative paths and are uploaded once.
pub fn additional_par2_paths(paths: &[String], source_paths: &[String]) -> Vec<String> {
    let key = |p: &str| clean_parts(p).join("/") + if p.starts_with('/') { "#abs" } else { "" };
    let mut seen: std::collections::HashSet<String> = source_paths.iter().map(|p| key(p)).collect();
    paths
        .iter()
        .filter(|p| seen.insert(key(p)))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fi(p: &str) -> FileInfo {
        FileInfo {
            path: p.into(),
            ..Default::default()
        }
    }

    #[test]
    fn rel_matches_go() {
        for (b, t, want) in [
            ("/a/b", "/a/b", Some(".")),
            ("/a/b", "/a/b/c/d", Some("c/d")),
            ("/a/b", "/a/c", Some("../c")),
            ("/a/b/c", "/a", Some("../..")),
            ("/a/b/", "/a/./b/x", Some("x")),
            ("a", "a/b", Some("b")),
            ("/a", "b", None),
            ("a", "/b", None),
            ("..", "a", None),
            ("/", "/x", Some("x")),
        ] {
            assert_eq!(filepath_rel(b, t).as_deref(), want, "Rel({b:?}, {t:?})");
        }
    }

    #[test]
    fn relative_path_from_is_empty_for_the_root_itself() {
        assert_eq!(relative_path_from("/root", "/root/f.bin"), "");
        assert_eq!(
            relative_path_from("/root", "/root/sub/deep/f.bin"),
            "sub/deep"
        );
        assert_eq!(relative_path_from("/root", "/other/f.bin"), "../other");
        assert_eq!(relative_path_from("rel", "/abs/f.bin"), "");
    }

    #[test]
    fn folder_name_prefers_the_first_subfolder() {
        assert_eq!(
            derive_folder_name("/watch", &[fi("/watch/Show/ep1.mkv")]),
            "Show"
        );
        assert_eq!(
            derive_folder_name("/watch/Show", &[fi("/watch/Show/ep1.mkv")]),
            "Show"
        );
        assert_eq!(derive_folder_name("/watch", &[]), "watch");
        assert_eq!(derive_folder_name("/", &[]), "upload");
        assert_eq!(
            derive_folder_name("/watch/a", &[fi("/watch/b/f.mkv")]),
            "..",
            "Go keeps a leading .. segment"
        );
    }

    #[test]
    fn additional_par2_paths_drops_sources_and_duplicates() {
        let got = additional_par2_paths(
            &[
                "/a/x.par2".into(),
                "/a/./x.par2".into(),
                "/a/y.par2".into(),
                "/a/z.par2".into(),
            ],
            &["/a/y.par2".into()],
        );
        assert_eq!(got, ["/a/x.par2", "/a/z.par2"]);
    }
}
