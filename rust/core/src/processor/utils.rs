//! Port of `utils.go`: path helpers used by the processor.

use std::path::{Component, Path, PathBuf};

/// True when `path` is a symbolic link (does not follow it).
pub fn is_symlink(path: &Path) -> std::io::Result<bool> {
    Ok(std::fs::symlink_metadata(path)?.file_type().is_symlink())
}

/// Lexically cleans a path like Go's `filepath.Clean` (resolves `.` and `..` without touching disk).
fn clean(p: &Path) -> PathBuf {
    let mut out: Vec<Component<'_>> = Vec::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                _ => out.push(c),
            },
            other => out.push(other),
        }
    }
    if out.is_empty() {
        PathBuf::from(".")
    } else {
        out.iter().collect()
    }
}

fn absolute(p: &Path) -> Option<PathBuf> {
    let c = clean(p);
    if c.is_absolute() {
        Some(c)
    } else {
        std::env::current_dir().ok().map(|d| clean(&d.join(c)))
    }
}

/// True when `file_path` is inside `dir_path` (lexically, both made absolute first). An empty directory
/// never contains anything.
pub fn is_within_path(file_path: &str, dir_path: &str) -> bool {
    if dir_path.is_empty() {
        return false;
    }
    let (Some(f), Some(d)) = (
        absolute(Path::new(file_path)),
        absolute(Path::new(dir_path)),
    ) else {
        return false;
    };
    match f.strip_prefix(&d) {
        Ok(rest) => !rest.components().any(|c| matches!(c, Component::ParentDir)),
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn within_path_cases() {
        for (f, d, want) in [
            ("/a/b/c.txt", "/a/b", true),
            ("/a/b", "/a/b", true),
            ("/a/b/../c", "/a/b", false),
            ("/a/bc/x", "/a/b", false),
            ("/a/b/./c/../d", "/a/b", true),
            ("/x", "", false),
            ("/a/b/c", "/a/b/", true),
            ("/other", "/a", false),
            ("/a", "/a/b", false),
        ] {
            assert_eq!(is_within_path(f, d), want, "{f} in {d}");
        }
    }

    #[test]
    fn relative_paths_resolve_against_the_working_directory() {
        let cwd = std::env::current_dir().unwrap();
        assert!(is_within_path(cwd.join("sub/f").to_str().unwrap(), "."));
        assert!(is_within_path("x/y", "x"));
        assert!(!is_within_path("../outside", "."));
    }

    #[test]
    fn detects_symlinks_without_following_them() {
        let d = std::env::temp_dir().join(format!(
            "sym-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&d).unwrap();
        let (file, link) = (d.join("f"), d.join("l"));
        std::fs::write(&file, b"x").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&file, &link).unwrap();
            assert!(is_symlink(&link).unwrap());
        }
        assert!(!is_symlink(&file).unwrap());
        assert!(is_symlink(&d.join("missing")).is_err());
    }
}
