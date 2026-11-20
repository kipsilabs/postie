use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use rusqlite::params;

use super::*;
use crate::db::Db;
use crate::manifest::Writer;

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "verif-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct FakeStater {
    missing: Mutex<HashSet<String>>,
    errored: Mutex<HashSet<String>>,
}
impl FakeStater {
    fn new(missing: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            missing: Mutex::new(missing.iter().map(|s| s.to_string()).collect()),
            ..Default::default()
        })
    }
    fn mark_present(&self, id: &str) {
        self.missing.lock().unwrap().remove(id);
    }
}
impl Stater for FakeStater {
    fn stat<'a>(&'a self, id: &'a str) -> BoxFut<'a, std::result::Result<bool, String>> {
        Box::pin(async move {
            if self.errored.lock().unwrap().contains(id) {
                return Err("stat timeout".into());
            }
            Ok(self.missing.lock().unwrap().contains(id))
        })
    }
    fn stat_batch<'a>(
        &'a self,
        ids: &'a [String],
    ) -> BoxFut<'a, std::result::Result<HashSet<String>, String>> {
        Box::pin(async move {
            let mut out = HashSet::new();
            for id in ids {
                if !matches!(self.stat(id).await, Ok(false)) {
                    out.insert(id.clone());
                }
            }
            Ok(out)
        })
    }
}

#[derive(Default)]
struct FakeReposter {
    reposted: Mutex<Vec<String>>,
    fail: Mutex<Option<RepostError>>,
    stater: Mutex<Option<Arc<FakeStater>>>,
}
impl Reposter for FakeReposter {
    fn repost<'a>(
        &'a self,
        rec: &'a ArticleRecord,
    ) -> BoxFut<'a, std::result::Result<(), RepostError>> {
        Box::pin(async move {
            if let Some(e) = self.fail.lock().unwrap().clone() {
                return Err(e);
            }
            self.reposted.lock().unwrap().push(rec.message_id.clone());
            if let Some(s) = self.stater.lock().unwrap().as_ref() {
                s.mark_present(&rec.message_id);
            }
            Ok(())
        })
    }
}

#[derive(Default)]
struct FakeCleaner(Mutex<Vec<String>>);
impl Cleaner for FakeCleaner {
    fn cleanup_transfer<'a>(
        &'a self,
        id: &'a str,
    ) -> BoxFut<'a, std::result::Result<bool, String>> {
        Box::pin(async move {
            self.0.lock().unwrap().push(id.to_string());
            Ok(true)
        })
    }
}

struct Env {
    tmp: Tmp,
    store: Store,
    db: Db,
    stater: Arc<FakeStater>,
    reposter: Arc<FakeReposter>,
    cleaner: Arc<FakeCleaner>,
    svc: Service,
    now: DateTime<Utc>,
}

fn cfg() -> Config {
    Config {
        propagation_delay: Duration::seconds(10),
        max_reposts: 2,
        deferred_backoff: Duration::seconds(30),
        max_backoff: Duration::minutes(5),
        max_deferred_checks: 3,
        stat_batch_size: 4,
        ..Default::default()
    }
}

fn env(missing: &[&str]) -> Env {
    let db = Db::in_memory().unwrap();
    db.lock().execute_batch("PRAGMA foreign_keys = ON").unwrap();
    crate::database::MigrationRunner::new(db.clone())
        .migrate_up()
        .unwrap();
    let store = Store::new(db.clone());
    let stater = FakeStater::new(missing);
    let reposter = Arc::new(FakeReposter::default());
    *reposter.stater.lock().unwrap() = Some(stater.clone());
    let cleaner = Arc::new(FakeCleaner::default());
    let mut svc = Service::new(store.clone(), stater.clone(), reposter.clone(), cfg(), "w1");
    svc.set_cleaner(cleaner.clone());
    Env {
        tmp: Tmp::new(),
        store,
        db,
        stater,
        reposter,
        cleaner,
        svc,
        now: Utc::now(),
    }
}

impl Env {
    fn record(i: i64) -> ArticleRecord {
        ArticleRecord {
            index: i,
            source_path: "/src/a.bin".into(),
            file_role: "original".into(),
            offset: i * 1000,
            body_size: 1000,
            message_id: format!("m{i}"),
            subject: "s".into(),
            original_subject: "os".into(),
            from: "f <f@x.y>".into(),
            groups: vec!["alt.test".into()],
            date: chrono::DateTime::parse_from_rfc3339("2023-11-14T22:13:20Z").unwrap(),
            custom_headers: BTreeMap::new(),
            xnxg_header: String::new(),
            file_name: "a.bin".into(),
            part_number: i + 1,
            total_parts: 10,
            file_size: 10_000,
        }
    }

    /// An uploaded file with `n` manifest records, due for its first check; linked to item `item`.
    fn file(&self, transfer: &str, file: &str, n: i64, item: &str) -> TransferFile {
        let path = crate::manifest::file_path(&self.tmp.0, transfer, file);
        let mut w = Writer::new(&path).unwrap();
        for i in 0..n {
            w.write(&Self::record(i)).unwrap();
        }
        w.commit().unwrap();
        let tf = TransferFile {
            transfer_id: transfer.into(),
            file_id: file.into(),
            manifest_path: path.to_string_lossy().into_owned(),
            source_path: "/src/a.bin".into(),
            file_role: "original".into(),
            article_count: n,
            completed_item_id: item.into(),
            ..Default::default()
        };
        self.store.upsert_file(&tf).unwrap();
        self.store
            .mark_uploaded(
                transfer,
                file,
                self.now - Duration::minutes(5),
                self.now - Duration::minutes(1),
            )
            .unwrap();
        self.store.get_file(transfer, file).unwrap().unwrap()
    }

    fn item(&self, id: &str, status: &str) {
        self.db
            .lock()
            .execute("INSERT INTO completed_items (id, path, size, nzb_path, created_at, job_data, verification_status) VALUES (?1,'/p',1,'/n.nzb','2026-01-01T00:00:00Z','{}',?2)", params![id, status])
            .unwrap();
    }

    fn item_status(&self, id: &str) -> String {
        self.db
            .lock()
            .query_row(
                "SELECT verification_status FROM completed_items WHERE id=?1",
                params![id],
                |r| r.get(0),
            )
            .unwrap()
    }

    fn state(&self, t: &str, f: &str) -> String {
        self.store
            .get_file(t, f)
            .unwrap()
            .unwrap()
            .verification_state
    }

    fn cancel() -> CancellationToken {
        CancellationToken::new()
    }
}

#[tokio::test]
async fn all_present_marks_the_file_verified_updates_the_item_and_cleans_up() {
    let e = env(&[]);
    e.item("ci", "pending_verification");
    let tf = e.file("t", "f", 10, "ci");
    e.svc.verify_file(&Env::cancel(), &tf).await.unwrap();
    assert_eq!(e.state("t", "f"), ts::STATE_VERIFIED);
    assert_eq!(e.item_status("ci"), "verified");
    assert_eq!(e.cleaner.0.lock().unwrap().as_slice(), ["t"]);
}

#[tokio::test]
async fn misses_are_persisted_and_no_cleanup_runs() {
    let e = env(&["m2", "m7"]);
    e.item("ci", "pending_verification");
    let tf = e.file("t", "f", 10, "ci");
    e.svc.verify_file(&Env::cancel(), &tf).await.unwrap();
    assert_eq!(e.state("t", "f"), ts::STATE_VERIFYING);
    assert_eq!(
        e.store
            .count_failures("t", "f", ts::FAILURE_PENDING)
            .unwrap(),
        2
    );
    let next = e
        .store
        .get_file("t", "f")
        .unwrap()
        .unwrap()
        .next_check_at
        .unwrap();
    assert!(
        next > Utc::now() + Duration::seconds(5),
        "the recheck waits for propagation"
    );
    assert!(e.cleaner.0.lock().unwrap().is_empty());
    assert_eq!(e.item_status("ci"), "pending_verification");
}

#[tokio::test]
async fn a_repost_is_followed_by_a_recheck_that_resolves() {
    let e = env(&["m3"]);
    e.item("ci", "pending_verification");
    let tf = e.file("t", "f", 5, "ci");
    e.svc.verify_file(&Env::cancel(), &tf).await.unwrap();
    let later = Utc::now() + Duration::minutes(1);
    assert_eq!(
        e.svc
            .process_due_failures(&Env::cancel(), later)
            .await
            .unwrap(),
        1
    );
    assert_eq!(e.reposter.reposted.lock().unwrap().as_slice(), ["m3"]);
    let claimed = e
        .store
        .claim_due_failures("x", Duration::minutes(1), 10, later + Duration::minutes(1))
        .unwrap();
    assert_eq!((claimed.len(), claimed[0].repost_count), (1, 1));
    assert!(
        claimed[0].next_attempt_at > Utc::now() + Duration::seconds(5),
        "propagation starts after the repost finishes"
    );
    // the fake reposter made it present: the next cycle resolves and verifies the file
    e.store
        .reclaim_expired_leases(later + Duration::hours(1))
        .unwrap();
    assert_eq!(
        e.svc
            .process_due_failures(&Env::cancel(), later + Duration::hours(1))
            .await
            .unwrap(),
        1
    );
    assert_eq!(e.state("t", "f"), ts::STATE_VERIFIED);
    assert_eq!(e.item_status("ci"), "verified");
    assert_eq!(
        e.reposter.reposted.lock().unwrap().len(),
        1,
        "only one repost"
    );
}

#[tokio::test]
async fn propagation_lag_resolves_without_any_repost() {
    let e = env(&["m1"]);
    let tf = e.file("t", "f", 3, "");
    e.svc.verify_file(&Env::cancel(), &tf).await.unwrap();
    e.stater.mark_present("m1"); // it showed up in the meantime
    e.svc
        .process_due_failures(&Env::cancel(), Utc::now() + Duration::minutes(1))
        .await
        .unwrap();
    assert!(e.reposter.reposted.lock().unwrap().is_empty());
    assert_eq!(e.state("t", "f"), ts::STATE_VERIFIED);
}

#[tokio::test]
async fn a_failed_stat_check_does_not_consume_budget() {
    let e = env(&["m1"]);
    let tf = e.file("t", "f", 3, "");
    e.svc.verify_file(&Env::cancel(), &tf).await.unwrap();
    e.stater.errored.lock().unwrap().insert("m1".into());
    let t = Utc::now() + Duration::minutes(1);
    e.svc.process_due_failures(&Env::cancel(), t).await.unwrap();
    let f = e
        .store
        .claim_due_failures("x", Duration::minutes(1), 10, t + Duration::hours(1))
        .unwrap()
        .remove(0);
    assert_eq!((f.repost_count, f.deferred_count), (0, 0));
    assert!(
        f.last_error.starts_with("stat check failed"),
        "{}",
        f.last_error
    );
    assert!(e.reposter.reposted.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_missing_repost_source_falls_back_to_bounded_stat_rechecks() {
    let e = env(&["m1"]);
    *e.reposter.fail.lock().unwrap() = Some(RepostError::SourceMissing("gone".into()));
    let tf = e.file("t", "f", 3, "");
    e.svc.verify_file(&Env::cancel(), &tf).await.unwrap();
    let t = Utc::now() + Duration::minutes(1);
    e.svc.process_due_failures(&Env::cancel(), t).await.unwrap();
    let f = e
        .store
        .claim_due_failures("x", Duration::minutes(1), 10, t + Duration::hours(1))
        .unwrap()
        .remove(0);
    assert_eq!(
        (f.repost_count, f.deferred_count, f.last_error.as_str()),
        (0, 1, "article still missing")
    );
}

#[tokio::test]
async fn a_transient_repost_error_consumes_an_attempt() {
    let e = env(&["m1"]);
    *e.reposter.fail.lock().unwrap() = Some(RepostError::Other("boom".into()));
    let tf = e.file("t", "f", 3, "");
    e.svc.verify_file(&Env::cancel(), &tf).await.unwrap();
    let t = Utc::now() + Duration::minutes(1);
    e.svc.process_due_failures(&Env::cancel(), t).await.unwrap();
    let f = e
        .store
        .claim_due_failures("x", Duration::minutes(1), 10, t + Duration::hours(1))
        .unwrap()
        .remove(0);
    assert_eq!(f.repost_count, 1);
    assert_eq!(f.last_error, "repost failed: boom");
}

#[tokio::test]
async fn exhausted_rechecks_fail_the_article_the_file_and_the_item() {
    let e = env(&["m1"]);
    e.item("ci", "pending_verification");
    *e.reposter.fail.lock().unwrap() = Some(RepostError::SourceMissing("gone".into()));
    let tf = e.file("t", "f", 3, "ci");
    e.svc.verify_file(&Env::cancel(), &tf).await.unwrap();
    let mut t = Utc::now() + Duration::minutes(1);
    for _ in 0..3 {
        t += Duration::hours(1);
        e.store.reclaim_expired_leases(t).unwrap();
        e.svc.process_due_failures(&Env::cancel(), t).await.unwrap();
    }
    assert_eq!(
        e.store
            .count_failures("t", "f", ts::FAILURE_FAILED)
            .unwrap(),
        1
    );
    assert_eq!(e.state("t", "f"), ts::STATE_VERIFICATION_FAILED);
    assert_eq!(e.item_status("ci"), "verification_failed");
    assert!(
        e.cleaner.0.lock().unwrap().is_empty(),
        "nothing is cleaned up after a failure"
    );
}

#[tokio::test]
async fn a_missing_manifest_fails_the_file_terminally() {
    let e = env(&[]);
    e.item("ci", "pending_verification");
    let tf = e.file("t", "f", 3, "ci");
    std::fs::remove_file(&tf.manifest_path).unwrap();
    e.svc.verify_file(&Env::cancel(), &tf).await.unwrap();
    let f = e.store.get_file("t", "f").unwrap().unwrap();
    assert_eq!(f.verification_state, ts::STATE_VERIFICATION_FAILED);
    assert!(
        f.last_error.starts_with("manifest missing:"),
        "{}",
        f.last_error
    );
    assert_eq!(e.item_status("ci"), "verification_failed");
}

#[tokio::test]
async fn a_corrupt_manifest_defers_with_backoff_then_fails() {
    let e = env(&[]);
    let tf = e.file("t", "f", 3, "");
    std::fs::write(&tf.manifest_path, b"not zstd at all").unwrap();
    let mut cur = e.store.get_file("t", "f").unwrap().unwrap();
    for attempt in 1..=2 {
        e.svc.verify_file(&Env::cancel(), &cur).await.unwrap();
        cur = e.store.get_file("t", "f").unwrap().unwrap();
        assert_eq!(
            (cur.check_attempts, cur.verification_state.as_str()),
            (attempt, ts::STATE_UPLOADED),
            "attempt {attempt} backs off"
        );
        assert!(
            cur.next_check_at.unwrap()
                > Utc::now() + Duration::seconds(20) * (1 << (attempt - 1)) / 2
        );
    }
    e.svc.verify_file(&Env::cancel(), &cur).await.unwrap();
    let f = e.store.get_file("t", "f").unwrap().unwrap();
    assert_eq!(f.verification_state, ts::STATE_VERIFICATION_FAILED);
    assert!(
        f.last_error
            .starts_with("verification check failed after repeated attempts:")
    );
}

#[tokio::test]
async fn finalize_waits_until_the_item_is_linked() {
    let e = env(&[]);
    e.item("ci", "pending_verification");
    let tf = e.file("t", "f", 3, ""); // not linked yet
    e.svc.verify_file(&Env::cancel(), &tf).await.unwrap();
    assert_eq!(e.state("t", "f"), ts::STATE_VERIFIED);
    assert_eq!(e.item_status("ci"), "pending_verification");
    assert!(
        e.cleaner.0.lock().unwrap().is_empty(),
        "no cleanup for an unlinked item"
    );
    e.store.set_completed_item_for_transfer("t", "ci").unwrap();
    e.svc.reconcile_stuck(&Env::cancel()).await.unwrap();
    assert_eq!(e.item_status("ci"), "verified");
    assert_eq!(e.cleaner.0.lock().unwrap().as_slice(), ["t"]);
}

#[tokio::test]
async fn due_files_are_the_uploaded_ones_whose_time_has_come() {
    let e = env(&[]);
    e.file("t1", "due", 2, "");
    let later = e.file("t2", "later", 2, "");
    e.store
        .mark_uploaded("t2", "later", Utc::now(), Utc::now() + Duration::hours(1))
        .unwrap();
    e.store
        .upsert_file(&TransferFile {
            transfer_id: "t3".into(),
            file_id: "planned".into(),
            manifest_path: later.manifest_path.clone(),
            source_path: "s".into(),
            ..Default::default()
        })
        .unwrap();
    let n = e
        .svc
        .verify_due_files(&Env::cancel(), Utc::now(), 10)
        .await
        .unwrap();
    assert_eq!(n, 1);
    assert_eq!(
        (
            e.state("t1", "due").as_str(),
            e.state("t2", "later").as_str()
        ),
        (ts::STATE_VERIFIED, ts::STATE_UPLOADED)
    );
    assert_eq!(
        e.svc
            .process_due_failures(&Env::cancel(), Utc::now())
            .await
            .unwrap(),
        0,
        "nothing due"
    );
}

#[tokio::test]
async fn the_busy_gate_defers_cycles_but_forces_one_after_the_maximum_skips() {
    let mut e = env(&["m1"]);
    let tf = e.file("t", "f", 3, "");
    let busy = Arc::new(AtomicBool::new(true));
    let b2 = busy.clone();
    e.svc
        .set_busy_check(Arc::new(move || b2.load(Ordering::SeqCst)));
    for _ in 0..MAX_BUSY_SKIPS {
        e.svc.run_once(&Env::cancel()).await;
        assert_eq!(e.state("t", "f"), ts::STATE_UPLOADED, "deferred while busy");
    }
    e.svc.run_once(&Env::cancel()).await; // forced
    assert_eq!(e.state("t", "f"), ts::STATE_VERIFYING);
    let _ = tf;
}

#[tokio::test]
async fn reconcile_handles_orphans_legacy_failures_and_terminal_files() {
    let e = env(&[]);
    // an orphan with nothing outstanding becomes verified
    e.item("orphan", "pending_verification");
    // a legacy item with a pending STAT-only failure is left alone
    e.item("legacy-pending", "pending_verification");
    e.store
        .add_failure(&VerificationFailure {
            transfer_id: "legacy-pending".into(),
            file_id: ts::LEGACY_FILE_ID.into(),
            article_index: -1,
            message_id: "<l1>".into(),
            ..Default::default()
        })
        .unwrap();
    // a legacy item whose failure exhausted becomes failed
    e.item("legacy-failed", "pending_verification");
    e.store
        .add_failure(&VerificationFailure {
            transfer_id: "legacy-failed".into(),
            file_id: ts::LEGACY_FILE_ID.into(),
            article_index: -1,
            message_id: "<l2>".into(),
            state: ts::FAILURE_FAILED.into(),
            ..Default::default()
        })
        .unwrap();
    // terminal files that were never finalized are finalized; an active one is not
    e.item("terminal", "pending_verification");
    e.file("tt", "f", 2, "terminal");
    e.store
        .set_verification_state("tt", "f", ts::STATE_VERIFIED, None, "")
        .unwrap();
    e.item("active", "pending_verification");
    e.file("ta", "f", 2, "active"); // still `uploaded`
    e.svc.reconcile_stuck(&Env::cancel()).await.unwrap();
    assert_eq!(e.item_status("orphan"), "verified");
    assert_eq!(e.item_status("legacy-pending"), "pending_verification");
    assert_eq!(e.item_status("legacy-failed"), "verification_failed");
    assert_eq!(e.item_status("terminal"), "verified");
    assert_eq!(e.item_status("active"), "pending_verification");
}

#[tokio::test]
async fn a_large_manifest_streams_correctly_in_batches() {
    let e = env(&["m4999", "m17"]);
    let tf = e.file("t", "f", 5000, "");
    e.svc.verify_file(&Env::cancel(), &tf).await.unwrap();
    assert_eq!(
        e.store
            .count_failures("t", "f", ts::FAILURE_PENDING)
            .unwrap(),
        2
    );
}

#[test]
fn backoff_doubles_up_to_the_cap() {
    let e = env(&[]);
    let secs = |n| e.svc.backoff(n).num_seconds();
    assert_eq!((secs(0), secs(1), secs(2), secs(3)), (30, 60, 120, 240));
    assert_eq!((secs(4), secs(40)), (300, 300), "capped at max_backoff");
}
