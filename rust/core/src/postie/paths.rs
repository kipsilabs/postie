//! Path helpers of `pkg/postie`: Go's `filepath.Rel` semantics plus the folder/relative-path rules.

use std::path::Path;

use crate::fileinfo::FileInfo;

fn is_sep(c: char) -> bool {
    c == '/' || (cfg!(windows) && c == '\\')
}

/// Splits a Windows drive prefix (`C:`) off; elsewhere the whole path is the rest.
fn volume(p: &str) -> (String, &str) {
    let b = p.as_bytes();
    if cfg!(windows) && b.len() >= 2 && b[1] == b':' && b[0].is_ascii_alphabetic() {
        return (p[..2].to_ascii_uppercase(), &p[2..]);
    }
    (String::new(), p)
}

fn rooted(rest: &str) -> bool {
    rest.starts_with(is_sep)
}

fn clean_parts(rest: &str) -> Vec<String> {
    let abs = rooted(rest);
    let mut out: Vec<String> = Vec::new();
    for part in rest.split(is_sep) {
        match part {
            "" | "." => {}
            ".." => {
                if out.last().is_some_and(|l| l != "..") {
                    out.pop();
                } else if !abs {
                    out.push("..".into());
                }
            }
            other => out.push(other.to_string()),
        }
    }
    out
}

/// Go's `filepath.Clean`: separators become the platform's, `.`/`..`/empty elements collapse.
pub fn clean_path(p: &str) -> String {
    let (vol, rest) = volume(p);
    let sep = if cfg!(windows) { "\\" } else { "/" };
    let joined = clean_parts(rest).join(sep);
    match (rooted(rest), joined.is_empty()) {
        (true, _) => format!("{vol}{sep}{joined}"),
        (false, true) => format!("{vol}."),
        (false, false) => format!("{vol}{joined}"),
    }
}

/// Go's `filepath.Rel(base, target)`: `.` when equal, `../x` for siblings, `None` when the volumes
/// differ, one path is rooted and the other is not, or `base` climbs above the common root.
pub fn filepath_rel(base: &str, target: &str) -> Option<String> {
    let ((bv, br), (tv, tr)) = (volume(base), volume(target));
    if !bv.eq_ignore_ascii_case(&tv) || rooted(br) != rooted(tr) {
        return None;
    }
    let (b, t) = (clean_parts(br), clean_parts(tr));
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

/// Whether `file` lies strictly inside `root` (both cleaned or not).
pub fn is_strictly_inside(root: &str, file: &str) -> bool {
    filepath_rel(root, file).is_some_and(|r| r != "." && r != ".." && !r.starts_with("../"))
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
    let key = |p: &str| {
        let (vol, rest) = volume(p);
        format!(
            "{vol}{}{}",
            clean_parts(rest).join("/"),
            if rooted(rest) { "#abs" } else { "" }
        )
    };
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

    #[cfg(unix)] // fixture uses Unix paths or sh
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

    #[cfg(unix)] // fixture uses Unix paths or sh
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

    #[cfg(windows)]
    #[test]
    fn windows_paths_use_volumes_and_backslashes() {
        assert_eq!(clean_path("C:/a//b/../c"), "C:\\a\\c");
        assert_eq!(clean_path("c:"), "C:.");
        assert_eq!(filepath_rel("C:\\a\\b", "c:/a/b/c").as_deref(), Some("c"));
        assert_eq!(filepath_rel("C:\\a", "D:\\a"), None);
        assert_eq!(filepath_rel("C:\\a", "b"), None);
        assert!(is_strictly_inside("C:\\root", "C:\\root\\sub\\f"));
        assert!(!is_strictly_inside("C:\\root", "C:\\root"));
    }
}
