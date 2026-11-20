//! Lifecycle of the temporary copies the web server makes of browser uploads.
//!
//! The server streams each upload into a fresh `postie-<digits>` (files) or `postie-folder-<digits>`
//! (folder) directory directly under the temp root (`par2.temp_dir`, or the OS temp dir). Those copies
//! are deleted after upload; this module removes what that leaves behind (empty directories) and
//! deletes copies whose queue item the user removed.
//!
//! SAFETY: everything here is gated on [`upload_root_of`]: a path qualifies only if it is lexically
//! (no `..`) inside a directory named exactly `postie-<digits>` / `postie-folder-<digits>` that sits
//! DIRECTLY in a known temp root, and (for deletion) no component from that directory down is a
//! symlink. Anything else (user folders, `postie-par2`, lookalike names) is never touched. Directory
//! pruning uses `remove_dir`, which fails on a non-empty directory, so files are never removed by it.

use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// Empty upload dirs older than this are swept. Generous: a huge folder upload keeps its (still empty)
/// directory for as long as the body takes to stream in.
pub const SWEEP_MIN_AGE: Duration = Duration::from_secs(24 * 3600);
/// How often the background sweep runs.
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(3600);

static EXTRA_ROOTS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// Makes a (possibly relative, e.g. `./temp`) root absolute against the current directory, so copies
/// created and recognised under it agree. Lexical: no symlink resolution.
pub fn absolute(p: &Path) -> PathBuf {
    std::path::absolute(p)
        .ok()
        .or_else(|| std::env::current_dir().ok().map(|d| d.join(p)))
        .unwrap_or_else(|| p.to_path_buf())
}

/// Remembers a temp root (the configured `par2.temp_dir`) so later cleanup recognises copies in it.
/// Roots only accumulate: widening this list is safe because the directory-name check still applies.
pub fn register_root(root: &Path) {
    if root.as_os_str().is_empty() {
        return;
    }
    let root = &absolute(root);
    let mut roots = EXTRA_ROOTS.lock().unwrap_or_else(|e| e.into_inner());
    if !roots.iter().any(|r| r == root) {
        roots.push(root.to_path_buf());
    }
}

/// The OS temp dir plus every registered root (each also in canonical form, for symlinked temp dirs).
pub fn candidate_roots() -> Vec<PathBuf> {
    let mut base = vec![std::env::temp_dir()];
    base.extend(
        EXTRA_ROOTS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned(),
    );
    let mut out = Vec::new();
    for r in base {
        if let Ok(c) = std::fs::canonicalize(&r) {
            out.push(c);
        }
        out.push(r);
    }
    out
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

/// `postie-<digits>` or `postie-folder-<digits>`: exactly what `temp_dir` in the web routes creates
/// for uploads that are queued.
pub fn is_upload_dir_name(name: &str) -> bool {
    name.strip_prefix("postie-folder-")
        .or_else(|| name.strip_prefix("postie-"))
        .is_some_and(all_digits)
}

/// Like [`is_upload_dir_name`] plus the `postie-stage-<digits>` staging dir (only ever swept if empty).
fn is_sweepable_dir_name(name: &str) -> bool {
    is_upload_dir_name(name) || name.strip_prefix("postie-stage-").is_some_and(all_digits)
}

/// Lexically normalises: drops `.` and rejects `..` (returns `None`).
fn normalize(p: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => return None,
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    Some(out)
}

/// Returns the upload root directory containing `path` (`path` may be the root itself only when
/// `allow_root`), or `None` when `path` is not a Postie web-upload temp copy.
fn upload_root_in(path: &Path, roots: &[PathBuf], allow_root: bool) -> Option<PathBuf> {
    if !path.is_absolute() {
        return None;
    }
    let path = normalize(path)?;
    for root in roots {
        let Some(root) = normalize(root).filter(|r| r.is_absolute()) else {
            continue;
        };
        let Ok(rel) = path.strip_prefix(&root) else {
            continue;
        };
        let mut comps = rel.components();
        let Some(Component::Normal(first)) = comps.next() else {
            continue;
        };
        if !first.to_str().is_some_and(is_upload_dir_name) {
            continue;
        }
        if comps.next().is_none() && !allow_root {
            continue;
        }
        return Some(root.join(first));
    }
    None
}

/// The upload root dir holding `path` when it is a web-upload temp copy (strictly inside the dir).
pub fn upload_root_of(path: &Path, roots: &[PathBuf]) -> Option<PathBuf> {
    upload_root_in(path, roots, false)
}

/// True when `path` is a Postie web-upload temp copy.
pub fn is_upload_copy(path: &Path, roots: &[PathBuf]) -> bool {
    upload_root_of(path, roots).is_some()
}

/// No component of `path` below `root` is a symlink, and `root` itself is a real directory.
fn no_symlinks_below(root: &Path, path: &Path) -> bool {
    let is_link = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink());
    if is_link(root) {
        return false;
    }
    let Some(rel) = normalize(path).and_then(|p| p.strip_prefix(root).ok().map(Path::to_path_buf))
    else {
        return false;
    };
    let mut cur = root.to_path_buf();
    for c in rel.components() {
        cur.push(c.as_os_str());
        if is_link(&cur) {
            return false;
        }
    }
    true
}

/// Removes now-empty directories from `start` up to and including the upload root. Stops at the first
/// non-empty one. Only acts when `start` lies in (or is) an upload root dir.
fn prune_empty_upward_in(start: &Path, roots: &[PathBuf]) {
    let Some(root) = upload_root_in(start, roots, true) else {
        return;
    };
    let Some(mut cur) = normalize(start) else {
        return;
    };
    while cur.starts_with(&root) {
        if std::fs::remove_dir(&cur).is_err() {
            return;
        }
        if cur == root {
            return;
        }
        if !cur.pop() {
            return;
        }
    }
}

/// Call after a copy at `deleted_file` was deleted: prunes its empty parent dirs up to the upload root.
pub fn prune_after_delete(deleted_file: &Path) {
    prune_after_delete_in(deleted_file, &candidate_roots());
}

fn prune_after_delete_in(deleted_file: &Path, roots: &[PathBuf]) {
    if !is_upload_copy(deleted_file, roots) {
        return;
    }
    if let Some(parent) = deleted_file.parent() {
        prune_empty_upward_in(parent, roots);
    }
}

/// Deletes a user-removed queue item's temp copy: a file, or (for a `FOLDER:` job) the folder tree, then
/// prunes empty parents. No-op unless `job_path` is an upload copy.
pub fn discard_copy(job_path: &str) {
    discard_copy_in(job_path, &candidate_roots());
}

fn discard_copy_in(job_path: &str, roots: &[PathBuf]) {
    let raw = job_path.strip_prefix("FOLDER:").unwrap_or(job_path);
    let is_folder = raw.len() != job_path.len();
    let path = Path::new(raw);
    // a folder job may point at the upload dir itself
    let Some(root) = upload_root_in(path, roots, is_folder) else {
        return;
    };
    let Some(path) = normalize(path) else {
        return;
    };
    if !no_symlinks_below(&root, &path) {
        eprintln!(
            "uploadtemp: refusing to delete copy behind a symlink: {}",
            path.display()
        );
        return;
    }
    let Ok(meta) = std::fs::symlink_metadata(&path) else {
        prune_empty_upward_in(path.parent().unwrap_or(&root), roots);
        return;
    };
    let res = if meta.is_dir() {
        // only folder jobs may remove a directory tree; a file job path that is a dir is left alone
        if !is_folder {
            return;
        }
        std::fs::remove_dir_all(&path)
    } else {
        std::fs::remove_file(&path)
    };
    if let Err(e) = res {
        eprintln!(
            "uploadtemp: could not delete removed upload copy: path={} error={e}",
            path.display()
        );
        return;
    }
    if path != root {
        prune_empty_upward_in(path.parent().unwrap_or(&root), roots);
    }
}

/// Removes empty subdirectories (post-order) and then `dir` itself if it ended up empty.
fn prune_tree(dir: &Path) -> bool {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return false;
    };
    let mut empty = true;
    for e in rd.flatten() {
        // `file_type` does not follow symlinks, so a link is "a file" and keeps the dir non-empty
        if e.file_type().is_ok_and(|t| t.is_dir()) && prune_tree(&e.path()) {
            continue;
        }
        empty = false;
    }
    empty && std::fs::remove_dir(dir).is_ok()
}

/// Removes upload dirs directly under `roots` that hold no files at all and were last modified at least
/// `min_age` ago. Returns how many were removed. Files are never deleted.
pub fn sweep_empty_dirs(roots: &[PathBuf], min_age: Duration) -> usize {
    let mut removed = 0;
    let mut seen = std::collections::HashSet::new();
    for root in roots {
        let Ok(rd) = std::fs::read_dir(root) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name();
            if !name.to_str().is_some_and(is_sweepable_dir_name)
                || !e.file_type().is_ok_and(|t| t.is_dir())
                || !seen.insert(e.path())
            {
                continue;
            }
            let old = e
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|t| t.elapsed().is_ok_and(|d| d >= min_age));
            if old && prune_tree(&e.path()) {
                removed += 1;
            }
        }
    }
    removed
}

/// [`sweep_empty_dirs`] over [`candidate_roots`] with [`SWEEP_MIN_AGE`].
pub fn sweep() -> usize {
    sweep_empty_dirs(&candidate_roots(), SWEEP_MIN_AGE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Scratch(PathBuf);
    impl Scratch {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "uploadtemp-test-{tag}-{}-{}",
                std::process::id(),
                rand::random::<u32>()
            ));
            fs::create_dir_all(&p).unwrap();
            Self(fs::canonicalize(p).unwrap())
        }
        fn roots(&self) -> Vec<PathBuf> {
            vec![self.0.clone()]
        }
        fn file(&self, rel: &str) -> PathBuf {
            let p = self.0.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, b"x").unwrap();
            p
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn dir_names() {
        for ok in ["postie-1", "postie-4294967295", "postie-folder-7"] {
            assert!(is_upload_dir_name(ok), "{ok}");
        }
        for bad in [
            "postie-",
            "postie-folder-",
            "postie-par2",
            "postie-stage-5",
            "postie-12a",
            "postie--1",
            "postie-folder-1-x",
            "Postie-1",
            "xpostie-1",
            "postie-1 ",
            "postie-folder",
            "",
            "postie-٣",
        ] {
            assert!(!is_upload_dir_name(bad), "{bad}");
        }
    }

    #[test]
    fn predicate_positive() {
        let s = Scratch::new("pos");
        let r = s.roots();
        assert!(is_upload_copy(&s.0.join("postie-123/a.mkv"), &r));
        assert!(is_upload_copy(&s.0.join("postie-folder-9/x/y/z.bin"), &r));
        assert!(is_upload_copy(&s.0.join("postie-folder-9/./x/z.bin"), &r));
        assert_eq!(
            upload_root_of(&s.0.join("postie-folder-9/x/z.bin"), &r),
            Some(s.0.join("postie-folder-9"))
        );
    }

    #[test]
    fn predicate_negative() {
        let s = Scratch::new("neg");
        let r = s.roots();
        let cases: Vec<PathBuf> = vec![
            s.0.join("postie-123"), // the dir itself is not a copy
            s.0.join("a.mkv"),
            s.0.join("movies/a.mkv"),
            s.0.join("postie-par2/work/a.par2"),
            s.0.join("postie-par2/postie-1/a"),
            s.0.join("postie-stage-1/0"),
            s.0.join("postie-abc/a"),
            s.0.join("postie-1x/a"),
            s.0.join("postie/a"),
            s.0.join("mine/postie-1/a"), // not DIRECTLY in the root
            s.0.join("postie-1/../../etc/passwd"),
            s.0.join("postie-1/../a.mkv"),
            s.0.join("postie-1/.."),
            PathBuf::from("postie-1/a.mkv"), // relative
            PathBuf::from("/"),
            PathBuf::from("/etc/passwd"),
            PathBuf::from("/home/u/postie-1/a"),
            s.0.parent().unwrap().join("postie-1/a"), // sibling of the root
        ];
        for c in cases {
            assert!(!is_upload_copy(&c, &r), "{}", c.display());
        }
        // no roots at all: nothing qualifies
        assert!(!is_upload_copy(&s.0.join("postie-1/a"), &[]));
    }

    #[test]
    fn prune_after_delete_removes_only_empty_dirs_up_to_root() {
        let s = Scratch::new("prune");
        let r = s.roots();
        let a = s.file("postie-folder-1/top/sub/a.bin");
        let b = s.file("postie-folder-1/top/b.bin");
        fs::create_dir_all(s.0.join("postie-folder-1/top/empty/deeper")).unwrap();
        fs::remove_file(&a).unwrap();
        prune_after_delete_in(&a, &r);
        assert!(!s.0.join("postie-folder-1/top/sub").exists());
        assert!(b.exists(), "sibling file survives");
        assert!(s.0.join("postie-folder-1/top").exists());
        fs::remove_file(&b).unwrap();
        prune_after_delete_in(&b, &r);
        // top still holds the unrelated empty dir, which prune_empty_upward does not descend into
        assert!(s.0.join("postie-folder-1/top").exists());
        // but the sweep clears whole empty trees
        assert_eq!(sweep_empty_dirs(&r, Duration::ZERO), 1);
        assert!(!s.0.join("postie-folder-1").exists());
        assert!(s.0.exists(), "the temp root itself is never removed");
    }

    #[test]
    fn prune_after_delete_removes_root_when_last_file_goes() {
        let s = Scratch::new("last");
        let r = s.roots();
        let a = s.file("postie-5/a.bin");
        let b = s.file("postie-5/b.bin");
        fs::remove_file(&a).unwrap();
        prune_after_delete_in(&a, &r);
        assert!(s.0.join("postie-5").exists(), "b still there");
        fs::remove_file(&b).unwrap();
        prune_after_delete_in(&b, &r);
        assert!(!s.0.join("postie-5").exists());
        assert!(s.0.exists());
    }

    #[test]
    fn non_upload_paths_are_never_touched() {
        let s = Scratch::new("user");
        let r = s.roots();
        let user = s.file("movies/keep.mkv");
        let par2 = s.file("postie-par2/work/a.par2");
        let look = s.file("postie-abc/keep.bin");
        let nested = s.file("mine/postie-1/keep.bin");
        fs::create_dir_all(s.0.join("emptyuser")).unwrap();
        for p in [&user, &par2, &look, &nested] {
            prune_after_delete_in(p, &r);
            discard_copy_in(&p.to_string_lossy(), &r);
            discard_copy_in(&format!("FOLDER:{}", p.parent().unwrap().display()), &r);
        }
        discard_copy_in(&format!("FOLDER:{}", s.0.join("movies").display()), &r);
        discard_copy_in(&format!("FOLDER:{}", s.0.display()), &r);
        // the temp root's empty user dir is not an upload dir name
        assert_eq!(sweep_empty_dirs(&r, Duration::ZERO), 0);
        for p in [&user, &par2, &look, &nested] {
            assert!(p.exists(), "{}", p.display());
        }
        assert!(s.0.join("emptyuser").exists());
    }

    #[test]
    fn discard_file_and_folder_copies() {
        let s = Scratch::new("discard");
        let r = s.roots();
        let a = s.file("postie-1/a.bin");
        let b = s.file("postie-1/b.bin");
        discard_copy_in(&a.to_string_lossy(), &r);
        assert!(!a.exists() && b.exists() && s.0.join("postie-1").exists());
        discard_copy_in(&b.to_string_lossy(), &r);
        assert!(!s.0.join("postie-1").exists());

        s.file("postie-folder-2/Album/d1/t.flac");
        s.file("postie-folder-2/Album/t2.flac");
        discard_copy_in(
            &format!("FOLDER:{}", s.0.join("postie-folder-2/Album").display()),
            &r,
        );
        assert!(!s.0.join("postie-folder-2").exists());

        // a folder job pointing at the upload dir itself
        s.file("postie-folder-3/x/y.bin");
        discard_copy_in(
            &format!("FOLDER:{}", s.0.join("postie-folder-3").display()),
            &r,
        );
        assert!(!s.0.join("postie-folder-3").exists());

        // a FILE job whose path is the upload dir (or a directory) is refused
        s.file("postie-4/keep.bin");
        discard_copy_in(&s.0.join("postie-4").to_string_lossy(), &r);
        assert!(s.0.join("postie-4/keep.bin").exists());
        // traversal out of the upload dir
        let user = s.file("movies/keep.mkv");
        discard_copy_in(
            &format!("{}/postie-4/../movies/keep.mkv", s.0.display()),
            &r,
        );
        discard_copy_in(&format!("FOLDER:{}/postie-4/../movies", s.0.display()), &r);
        assert!(user.exists());
    }

    #[test]
    fn sweep_respects_age_and_never_deletes_files() {
        let s = Scratch::new("sweep");
        let r = s.roots();
        fs::create_dir_all(s.0.join("postie-1/a/b")).unwrap();
        fs::create_dir_all(s.0.join("postie-stage-2")).unwrap();
        fs::create_dir_all(s.0.join("postie-par2/x")).unwrap();
        let full = s.file("postie-folder-3/a/f.bin");
        // too young
        assert_eq!(sweep_empty_dirs(&r, Duration::from_secs(3600)), 0);
        assert!(s.0.join("postie-1").exists());
        assert_eq!(sweep_empty_dirs(&r, Duration::ZERO), 2);
        assert!(!s.0.join("postie-1").exists());
        assert!(!s.0.join("postie-stage-2").exists());
        assert!(s.0.join("postie-par2/x").exists(), "work dir untouched");
        assert!(full.exists(), "dirs holding files untouched");
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_never_followed() {
        let s = Scratch::new("link");
        let r = s.roots();
        let user = s.file("movies/keep.mkv");
        // upload dir that is itself a symlink to a user dir
        std::os::unix::fs::symlink(s.0.join("movies"), s.0.join("postie-8")).unwrap();
        discard_copy_in(&s.0.join("postie-8/keep.mkv").to_string_lossy(), &r);
        discard_copy_in(&format!("FOLDER:{}", s.0.join("postie-8").display()), &r);
        assert!(user.exists());
        assert_eq!(sweep_empty_dirs(&r, Duration::ZERO), 0);
        // a symlinked subdirectory inside a real upload dir
        s.file("postie-9/real/f.bin");
        std::os::unix::fs::symlink(s.0.join("movies"), s.0.join("postie-9/lnk")).unwrap();
        discard_copy_in(&s.0.join("postie-9/lnk/keep.mkv").to_string_lossy(), &r);
        discard_copy_in(
            &format!("FOLDER:{}", s.0.join("postie-9/lnk").display()),
            &r,
        );
        assert!(user.exists());
    }

    #[test]
    fn relative_roots_are_absolutized_and_recognised() {
        let rel = Path::new("uploadtemp-rel-test-root");
        register_root(rel);
        let abs = absolute(rel);
        assert!(abs.is_absolute());
        assert!(candidate_roots().contains(&abs));
        let copy = abs.join("postie-77/a.bin");
        assert!(is_upload_copy(&copy, &candidate_roots()));
        // a relative path is still never a copy
        assert!(!is_upload_copy(
            Path::new("uploadtemp-rel-test-root/postie-77/a.bin"),
            &candidate_roots()
        ));
    }

    #[test]
    fn register_root_is_deduplicated() {
        let p = std::env::temp_dir().join("uploadtemp-register-test");
        register_root(&p);
        register_root(&p);
        register_root(Path::new(""));
        let n = candidate_roots().iter().filter(|r| **r == p).count();
        assert_eq!(n, 1);
    }
}
