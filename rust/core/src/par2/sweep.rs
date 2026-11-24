//! Port of `internal/par2sweep`: removes orphaned PAR2 files from Postie's PAR2 work directory.
//! Generated sets are normally deleted by the transfer cleaner once verified, but when the transfer
//! rows disappear first (a database reset, an old crash) nothing else removes them.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tokio_util::sync::CancellationToken;

use super::is_par2_file;

type Referenced = Arc<dyn Fn() -> Result<Vec<String>, String> + Send + Sync>;
type InUse = Arc<dyn Fn() -> Vec<String> + Send + Sync>;

/// Deletes PAR2 files in `dir` that are older than `grace`, not referenced by any transfer and not in
/// use by a running job. Only the top level of `dir` is looked at; it must be a directory Postie owns.
pub struct Sweeper {
    dir: String,
    grace: Duration,
    referenced: Option<Referenced>,
    in_use: Option<InUse>,
    now: Arc<dyn Fn() -> SystemTime + Send + Sync>,
}

impl Sweeper {
    /// `referenced` returns the source paths the durable store still tracks (`None` without a store);
    /// `in_use` the PAR2 files of currently running jobs.
    pub fn new(
        dir: &str,
        grace: Duration,
        referenced: Option<Referenced>,
        in_use: Option<InUse>,
    ) -> Self {
        Self {
            dir: dir.into(),
            grace,
            referenced,
            in_use,
            now: Arc::new(SystemTime::now),
        }
    }

    #[cfg(test)]
    fn with_clock(mut self, now: Arc<dyn Fn() -> SystemTime + Send + Sync>) -> Self {
        self.now = now;
        self
    }

    /// One pass; returns how many files were removed. A failing `referenced` lookup aborts the pass
    /// without deleting anything, since orphans cannot be told from sets still awaiting verification.
    pub fn sweep(&self, cancel: &CancellationToken) -> Result<usize, String> {
        if self.dir.is_empty() {
            return Ok(0);
        }
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(e) => e,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(e) => return Err(e.to_string()),
        };
        let mut keep: HashSet<String> = HashSet::new();
        if let Some(r) = &self.referenced {
            keep.extend(r()?);
        }
        if let Some(u) = &self.in_use {
            keep.extend(u());
        }
        let now = (self.now)();
        let cutoff = now
            .checked_sub(self.grace)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let mut removed = 0;
        for entry in entries.flatten() {
            if cancel.is_cancelled() {
                return Ok(removed);
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() || !is_par2_file(&name) {
                continue;
            }
            let path = Path::new(&self.dir)
                .join(&name)
                .to_string_lossy()
                .into_owned();
            if keep.contains(&path) {
                continue;
            }
            let Ok(mtime) = meta.modified() else { continue };
            if mtime >= cutoff {
                continue;
            }
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    eprintln!(
                        "par2sweep: failed to remove orphaned PAR2 file: path={path} error={e}"
                    );
                    continue;
                }
            }
            eprintln!("par2sweep: removed orphaned PAR2 file: path={path}");
            removed += 1;
        }
        Ok(removed)
    }

    /// Waits `initial_delay`, sweeps, then sweeps every `interval` until cancelled. The delay gives jobs
    /// resumed at startup time to reserve the sets they are about to reuse.
    pub async fn run(
        self: Arc<Self>,
        cancel: CancellationToken,
        initial_delay: Duration,
        interval: Duration,
    ) {
        tokio::select! {
            () = cancel.cancelled() => return,
            () = tokio::time::sleep(initial_delay) => {}
        }
        loop {
            let me = self.clone();
            let c = cancel.clone();
            let res = tokio::task::spawn_blocking(move || me.sweep(&c)).await;
            if let Ok(Err(e)) = res
                && !cancel.is_cancelled()
            {
                eprintln!(
                    "par2sweep: PAR2 work dir sweep failed: dir={} error={e}",
                    self.dir
                );
            }
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(interval) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "sweep-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn setup() -> (std::path::PathBuf, Vec<String>) {
        let d = dir();
        let mut paths = Vec::new();
        for n in [
            "old.par2",
            "old.vol0+1.PAR2",
            "kept-ref.par2",
            "kept-use.par2",
            "not-par.txt",
        ] {
            let p = d.join(n);
            std::fs::write(&p, b"x").unwrap();
            paths.push(p.to_string_lossy().into_owned());
        }
        std::fs::create_dir(d.join("sub.par2")).unwrap();
        (d, paths)
    }

    fn later() -> Arc<dyn Fn() -> SystemTime + Send + Sync> {
        Arc::new(|| SystemTime::now() + Duration::from_secs(7200))
    }

    #[test]
    fn removes_only_old_unreferenced_unused_par2_files() {
        let (d, p) = setup();
        let (r, u) = (p[2].clone(), p[3].clone());
        let s = Sweeper::new(
            d.to_str().unwrap(),
            Duration::from_secs(3600),
            Some(Arc::new(move || Ok(vec![r.clone()]))),
            Some(Arc::new(move || vec![u.clone()])),
        )
        .with_clock(later());
        assert_eq!(s.sweep(&CancellationToken::new()).unwrap(), 2);
        assert!(!d.join("old.par2").exists() && !d.join("old.vol0+1.PAR2").exists());
        assert!(d.join("kept-ref.par2").exists() && d.join("kept-use.par2").exists());
        assert!(d.join("not-par.txt").exists() && d.join("sub.par2").is_dir());
    }

    #[test]
    fn files_inside_the_grace_period_are_kept() {
        let (d, _) = setup();
        let s = Sweeper::new(d.to_str().unwrap(), Duration::from_secs(3600), None, None);
        assert_eq!(s.sweep(&CancellationToken::new()).unwrap(), 0);
        assert!(d.join("old.par2").exists());
    }

    #[test]
    fn a_failing_lookup_deletes_nothing() {
        let (d, _) = setup();
        let s = Sweeper::new(
            d.to_str().unwrap(),
            Duration::ZERO,
            Some(Arc::new(|| Err("db down".into()))),
            None,
        )
        .with_clock(later());
        assert_eq!(s.sweep(&CancellationToken::new()).unwrap_err(), "db down");
        assert!(d.join("old.par2").exists());
    }

    #[test]
    fn missing_or_empty_dir_is_a_no_op() {
        assert_eq!(
            Sweeper::new("", Duration::ZERO, None, None)
                .sweep(&CancellationToken::new())
                .unwrap(),
            0
        );
        assert_eq!(
            Sweeper::new("/nonexistent/zzz", Duration::ZERO, None, None)
                .sweep(&CancellationToken::new())
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn run_sweeps_after_the_initial_delay_and_stops_on_cancel() {
        let (d, _) = setup();
        let s = Arc::new(
            Sweeper::new(d.to_str().unwrap(), Duration::ZERO, None, None).with_clock(later()),
        );
        let cancel = CancellationToken::new();
        let h = tokio::spawn(s.run(
            cancel.clone(),
            Duration::from_millis(20),
            Duration::from_secs(3600),
        ));
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!d.join("old.par2").exists());
        cancel.cancel();
        h.await.unwrap();
    }
}
