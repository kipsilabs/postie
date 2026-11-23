//! Port of `internal/transfercleaner`: post-verification cleanup of a durable transfer. Once every
//! file is verified it runs the optional post-upload script, deletes originals whose policy asks for
//! it, removes generated PAR2 files (unless maintained) and removes the manifests.
//!
//! Safety: cleanup runs ONLY when every file reached a terminal state and NONE failed verification;
//! if any failed, nothing is deleted. It is idempotent: removing an already-removed file is fine.

use std::path::Path;
use std::sync::Arc;

use crate::manifest;
use crate::poster::BoxFut;
use crate::transferstore::{self as ts, Store, TransferFile};
use crate::verification::Cleaner;

/// Runs the post-upload script for a verified transfer.
pub type ScriptRunner = Arc<
    dyn for<'a> Fn(&'a str, &'a [TransferFile]) -> BoxFut<'a, Result<(), String>> + Send + Sync,
>;

pub struct TransferCleaner {
    store: Store,
    maintain_par2: bool,
    run_script: Option<ScriptRunner>,
}

impl TransferCleaner {
    /// With `maintain_par2`, generated PAR2 files are kept after verification.
    pub fn new(store: Store, maintain_par2: bool, run_script: Option<ScriptRunner>) -> Self {
        Self {
            store,
            maintain_par2,
            run_script,
        }
    }

    /// A re-added path may be queued or uploading again; on a lookup error keep the file.
    fn original_needed_elsewhere(&self, transfer_id: &str, path: &str) -> bool {
        if path.is_empty() {
            return false;
        }
        self.store
            .is_source_used_elsewhere(transfer_id, path)
            .unwrap_or_else(|e| {
                crate::logx::warn(
                    &format!("Could not check other uses of {path}: {e}"),
                    &[("component", "transfercleaner".into())],
                );
                true
            })
    }

    /// Cleans up a transfer if and only if every file is verified. Returns `true` only when cleanup ran
    /// to completion: a no-op while any file is pending/verifying, and everything is retained if any
    /// file failed verification.
    pub async fn cleanup_transfer(&self, transfer_id: &str) -> Result<bool, String> {
        let files = self
            .store
            .list_files_by_transfer(transfer_id)
            .map_err(|e| e.to_string())?;
        if files.is_empty() {
            return Ok(false);
        }
        for f in &files {
            match f.verification_state.as_str() {
                ts::STATE_VERIFIED => {}
                ts::STATE_VERIFICATION_FAILED => {
                    crate::logx::warn(
                        &format!(
                            "Transfer has a failed file; retaining all recovery data: transfer={transfer_id} file={}",
                            f.file_id
                        ),
                        &[("component", "transfercleaner".into())],
                    );
                    return Ok(false);
                }
                _ => return Ok(false),
            }
        }
        if let Some(run) = &self.run_script
            && let Err(e) = run(transfer_id, &files).await
        {
            crate::logx::warn(
                &format!(
                    "Post-upload script failed during cleanup: transfer={transfer_id} error={e}"
                ),
                &[("component", "transfercleaner".into())],
            );
        }
        for f in &files {
            match f.file_role.as_str() {
                manifest::ROLE_ORIGINAL if f.cleanup_policy == ts::CLEANUP_DELETE_ORIGINAL => {
                    if self.original_needed_elsewhere(transfer_id, &f.source_path) {
                        crate::logx::info(
                            &format!(
                                "Keeping original still used by another upload: transfer={transfer_id} path={}",
                                f.source_path
                            ),
                            &[("component", "transfercleaner".into())],
                        );
                    } else {
                        remove(&f.source_path, "original")
                    }
                }
                manifest::ROLE_GENERATED_PAR2 if !self.maintain_par2 => {
                    remove(&f.source_path, "generated par2")
                }
                _ => {}
            }
            // manifests are Postie's own recovery files; always removed once verified
            if let Err(e) = manifest::remove(Path::new(&f.manifest_path), transfer_id) {
                crate::logx::warn(
                    &format!(
                        "Failed to remove manifest during cleanup: path={} error={e}",
                        f.manifest_path
                    ),
                    &[("component", "transfercleaner".into())],
                );
            }
        }
        self.store
            .delete_files_by_transfer(transfer_id)
            .map_err(|e| e.to_string())?;
        crate::logx::info(
            &format!(
                "Transfer verified and cleaned up: transfer={transfer_id} files={}",
                files.len()
            ),
            &[("component", "transfercleaner".into())],
        );
        Ok(true)
    }
}

fn remove(path: &str, kind: &str) {
    if path.is_empty() {
        return;
    }
    if let Err(e) = std::fs::remove_file(path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        crate::logx::warn(
            &format!("Failed to remove file during cleanup: kind={kind} path={path} error={e}"),
            &[("component", "transfercleaner".into())],
        );
        return;
    }
    // web-upload temp copies leave empty `postie-*` dirs behind; no-op for any other path
    crate::uploadtemp::prune_after_delete(Path::new(path));
}

impl Cleaner for TransferCleaner {
    fn cleanup_transfer<'a>(&'a self, transfer_id: &'a str) -> BoxFut<'a, Result<bool, String>> {
        Box::pin(TransferCleaner::cleanup_transfer(self, transfer_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use std::sync::Mutex;

    struct Env {
        db: Db,
        store: Store,
        dir: std::path::PathBuf,
    }

    fn env() -> Env {
        let db = Db::in_memory().unwrap();
        db.lock().execute_batch("PRAGMA foreign_keys = ON").unwrap();
        crate::database::MigrationRunner::new(db.clone())
            .migrate_up()
            .unwrap();
        let dir = std::env::temp_dir().join(format!(
            "tc-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(dir.join("t1")).unwrap();
        Env {
            store: Store::new(db.clone()),
            db,
            dir,
        }
    }

    /// Adds a file row with real source and manifest files; returns (source, manifest).
    fn add(
        e: &Env,
        id: &str,
        role: &str,
        state: &str,
        policy: &str,
    ) -> (std::path::PathBuf, std::path::PathBuf) {
        let src = e.dir.join(format!("{id}.src"));
        let man = e.dir.join("t1").join(format!("{id}.jsonl.zst"));
        std::fs::write(&src, b"s").unwrap();
        std::fs::write(&man, b"m").unwrap();
        e.store
            .upsert_file(&TransferFile {
                transfer_id: "t1".into(),
                file_id: id.into(),
                manifest_path: man.to_string_lossy().into_owned(),
                source_path: src.to_string_lossy().into_owned(),
                file_role: role.into(),
                cleanup_policy: policy.into(),
                ..Default::default()
            })
            .unwrap();
        e.store
            .set_verification_state("t1", id, state, None, "")
            .unwrap();
        (src, man)
    }

    #[tokio::test]
    async fn all_verified_deletes_per_policy_and_drops_the_rows() {
        let e = env();
        let (orig, om) = add(&e, "o", "original", ts::STATE_VERIFIED, "delete_original");
        let (keep, km) = add(&e, "k", "original", ts::STATE_VERIFIED, "");
        let (par, pm) = add(&e, "p", "generated_par2", ts::STATE_VERIFIED, "");
        let c = TransferCleaner::new(e.store.clone(), false, None);
        assert!(c.cleanup_transfer("t1").await.unwrap());
        assert!(!orig.exists() && !par.exists() && keep.exists());
        assert!(!om.exists() && !km.exists() && !pm.exists());
        assert!(e.store.list_files_by_transfer("t1").unwrap().is_empty());
        assert!(
            !e.dir.join("t1").exists(),
            "the empty transfer directory goes too"
        );
        assert!(
            !c.cleanup_transfer("t1").await.unwrap(),
            "no rows left: no-op"
        );
    }

    #[tokio::test]
    async fn maintain_par2_keeps_generated_par2() {
        let e = env();
        let (par, _) = add(&e, "p", "generated_par2", ts::STATE_VERIFIED, "");
        assert!(
            TransferCleaner::new(e.store.clone(), true, None)
                .cleanup_transfer("t1")
                .await
                .unwrap()
        );
        assert!(par.exists());
    }

    #[tokio::test]
    async fn any_failed_file_retains_everything_and_skips_the_script() {
        let e = env();
        let (orig, om) = add(&e, "o", "original", ts::STATE_VERIFIED, "delete_original");
        add(&e, "f", "original", ts::STATE_VERIFICATION_FAILED, "");
        let ran = Arc::new(Mutex::new(0));
        let r = ran.clone();
        let script: ScriptRunner = Arc::new(move |_, _| {
            *r.lock().unwrap() += 1;
            Box::pin(async { Ok(()) })
        });
        assert!(
            !TransferCleaner::new(e.store.clone(), false, Some(script))
                .cleanup_transfer("t1")
                .await
                .unwrap()
        );
        assert!(orig.exists() && om.exists());
        assert_eq!(
            (
                *ran.lock().unwrap(),
                e.store.list_files_by_transfer("t1").unwrap().len()
            ),
            (0, 2)
        );
    }

    #[tokio::test]
    async fn unfinished_files_make_it_a_no_op() {
        let e = env();
        let (orig, _) = add(&e, "o", "original", ts::STATE_VERIFIED, "delete_original");
        add(&e, "u", "original", ts::STATE_VERIFYING, "");
        assert!(
            !TransferCleaner::new(e.store.clone(), false, None)
                .cleanup_transfer("t1")
                .await
                .unwrap()
        );
        assert!(orig.exists());
    }

    #[tokio::test]
    async fn the_script_runs_once_and_its_failure_does_not_block_cleanup() {
        let e = env();
        add(&e, "o", "original", ts::STATE_VERIFIED, "");
        let n = Arc::new(Mutex::new(Vec::new()));
        let n2 = n.clone();
        let script: ScriptRunner = Arc::new(move |t, files| {
            n2.lock().unwrap().push((t.to_string(), files.len()));
            Box::pin(async { Err("script exploded".to_string()) })
        });
        assert!(
            TransferCleaner::new(e.store.clone(), false, Some(script))
                .cleanup_transfer("t1")
                .await
                .unwrap()
        );
        assert_eq!(n.lock().unwrap().as_slice(), [("t1".to_string(), 1)]);
    }

    #[tokio::test]
    async fn already_removed_files_are_not_an_error() {
        let e = env();
        let (orig, _) = add(&e, "o", "original", ts::STATE_VERIFIED, "delete_original");
        std::fs::remove_file(&orig).unwrap();
        assert!(
            TransferCleaner::new(e.store.clone(), false, None)
                .cleanup_transfer("t1")
                .await
                .unwrap()
        );
    }

    fn other_transfer_row(e: &Env, src: &std::path::Path) {
        e.store
            .upsert_file(&TransferFile {
                transfer_id: "t2".into(),
                file_id: "o2".into(),
                manifest_path: e.dir.join("t2.jsonl.zst").to_string_lossy().into_owned(),
                source_path: src.to_string_lossy().into_owned(),
                file_role: "original".into(),
                ..Default::default()
            })
            .unwrap();
    }

    fn queue_job(e: &Env, path: &str) {
        let body = serde_json::json!({ "path": path }).to_string();
        e.db
            .lock()
            .execute(
                "INSERT INTO goqite (queue, body, timeout) VALUES ('file_jobs', CAST(?1 AS BLOB), '2000-01-01T00:00:00.000Z')",
                [body],
            )
            .unwrap();
    }

    async fn cleanup_keeps_original(e: &Env, orig: &std::path::Path, om: &std::path::Path) {
        let c = TransferCleaner::new(e.store.clone(), false, None);
        assert!(c.cleanup_transfer("t1").await.unwrap());
        assert!(orig.exists(), "original still needed elsewhere");
        assert!(!om.exists(), "own manifest still removed");
        assert!(e.store.list_files_by_transfer("t1").unwrap().is_empty());
    }

    #[tokio::test]
    async fn original_kept_when_another_transfer_uses_it() {
        let e = env();
        let (orig, om) = add(&e, "o", "original", ts::STATE_VERIFIED, "delete_original");
        other_transfer_row(&e, &orig);
        cleanup_keeps_original(&e, &orig, &om).await;
    }

    #[tokio::test]
    async fn original_kept_when_a_queue_job_or_folder_job_uses_it() {
        let e = env();
        let (orig, om) = add(&e, "o", "original", ts::STATE_VERIFIED, "delete_original");
        queue_job(&e, &orig.to_string_lossy());
        cleanup_keeps_original(&e, &orig, &om).await;

        let e = env();
        let (orig, om) = add(&e, "o", "original", ts::STATE_VERIFIED, "delete_original");
        queue_job(&e, &format!("FOLDER:{}", e.dir.to_string_lossy()));
        cleanup_keeps_original(&e, &orig, &om).await;
    }

    #[tokio::test]
    async fn original_kept_when_an_in_progress_job_uses_it() {
        let e = env();
        let (orig, om) = add(&e, "o", "original", ts::STATE_VERIFIED, "delete_original");
        e.db
            .lock()
            .execute(
                "INSERT INTO in_progress_items (id, path, size, priority, created_at, job_data) VALUES ('j', ?1, 1, 0, '2000-01-01T00:00:00Z', '{}')",
                [orig.to_string_lossy().into_owned()],
            )
            .unwrap();
        cleanup_keeps_original(&e, &orig, &om).await;
    }

    #[tokio::test]
    async fn unrelated_folder_job_does_not_block_deletion() {
        let e = env();
        let (orig, _) = add(&e, "o", "original", ts::STATE_VERIFIED, "delete_original");
        queue_job(
            &e,
            &format!("FOLDER:{}", e.dir.join("other").to_string_lossy()),
        );
        assert!(
            TransferCleaner::new(e.store.clone(), false, None)
                .cleanup_transfer("t1")
                .await
                .unwrap()
        );
        assert!(!orig.exists());
    }
}
