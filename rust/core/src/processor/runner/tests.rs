use std::sync::atomic::{AtomicU64, AtomicUsize};

use postie_nntp::NntpError;

use super::*;
use crate::config::{CustomHeader, NewsgroupConfig, PostHeaders};
use crate::db::Db;
use crate::poster::BoxFut;

#[derive(Default)]
struct Mock {
    fail_all: AtomicBool,
    delay_ms: AtomicU64,
    posted: AtomicUsize,
    ids: Mutex<std::collections::HashSet<String>>,
}

impl ArticleClient for Mock {
    fn post(&self, a: Vec<u8>) -> BoxFut<'_, Result<(), NntpError>> {
        Box::pin(async move {
            let d = self.delay_ms.load(Ordering::SeqCst);
            if d > 0 {
                tokio::time::sleep(Duration::from_millis(d)).await;
            }
            if self.fail_all.load(Ordering::SeqCst) {
                return Err(NntpError::Closed);
            }
            if let Some(id) = String::from_utf8_lossy(&a)
                .lines()
                .find_map(|l| l.strip_prefix("Message-ID: "))
            {
                self.ids
                    .lock()
                    .unwrap()
                    .insert(id.trim().trim_matches(['<', '>']).to_string());
            }
            self.posted.fetch_add(1, Ordering::SeqCst);
            Ok(())
        })
    }
    fn stat<'a>(&'a self, id: &'a str) -> BoxFut<'a, Result<bool, NntpError>> {
        Box::pin(async move {
            Ok(self
                .ids
                .lock()
                .unwrap()
                .contains(id.trim_matches(['<', '>'])))
        })
    }
    fn max_connections(&self) -> usize {
        4
    }
}

struct Pool {
    client: Arc<Mock>,
    health: Mutex<Vec<ProviderHealth>>,
    /// Number of initial `upload_client` calls that report no pool (a legacy, non-durable runtime).
    no_upload_calls: AtomicUsize,
}

impl PoolManager for Pool {
    #[allow(deprecated)] // `try_update` on newer toolchains
    fn upload_client(&self) -> Option<Arc<dyn ArticleClient>> {
        if self
            .no_upload_calls
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return None;
        }
        Some(self.client.clone())
    }
    fn verify_client(&self) -> Option<Arc<dyn ArticleClient>> {
        None
    }
    fn provider_health(&self) -> Result<Vec<ProviderHealth>, String> {
        Ok(self.health.lock().unwrap().clone())
    }
}

struct Env {
    p: Arc<Processor>,
    q: Queue,
    client: Arc<Mock>,
    pool: Arc<Pool>,
    src: PathBuf,
    errors: Arc<Mutex<Vec<(String, String)>>>,
    completes: Arc<AtomicUsize>,
}

fn tmp() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "proc-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn cfg(db_path: &Path) -> ConfigData {
    let mut c = ConfigData::default();
    c.posting.article_size_in_bytes = 1000;
    c.posting.max_retries = 3;
    c.posting.wait_for_par2 = Some(true);
    c.posting.groups = Some(vec![NewsgroupConfig {
        name: "alt.test".into(),
        enabled: Some(true),
    }]);
    c.posting.message_id_format = "random".into();
    c.posting.obfuscation_policy = "none".into();
    c.posting.par2_obfuscation_policy = "none".into();
    c.posting.group_policy = "all".into();
    c.posting.post_headers = PostHeaders {
        add_nxg_header: false,
        default_from: "T <t@example.com>".into(),
        custom_headers: Some(vec![CustomHeader {
            name: "X-T".into(),
            value: "1".into(),
        }]),
    };
    c.par2.enabled = Some(false);
    c.par2.gf16_method = "auto".into();
    c.maintain_original_extension = Some(true);
    c.database.database_path = db_path.to_string_lossy().into_owned();
    c
}

fn env_with(delete_original: bool, tweak: impl FnOnce(&mut ProcessorOptions)) -> Env {
    let db = Db::in_memory().unwrap();
    db.lock().execute_batch("PRAGMA foreign_keys = ON").unwrap();
    crate::database::MigrationRunner::new(db.clone())
        .migrate_up()
        .unwrap();
    let q = Queue::new(db);
    let (src, out) = (tmp(), tmp());
    let client = Arc::new(Mock::default());
    let pool = Arc::new(Pool {
        client: client.clone(),
        health: Mutex::new(vec![]),
        no_upload_calls: AtomicUsize::new(0),
    });
    let errors = Arc::new(Mutex::new(Vec::new()));
    let completes = Arc::new(AtomicUsize::new(0));
    let (e2, c2) = (errors.clone(), completes.clone());
    let mut opts = ProcessorOptions {
        queue: q.clone(),
        config: Arc::new(cfg(&out.join("db/postie.db"))),
        queue_config: QueueConfig {
            max_concurrent_uploads: 2,
            min_size_to_start: 0,
        },
        pool: Some(pool.clone()),
        output_folder: out.to_string_lossy().into_owned(),
        delete_original_file: delete_original,
        delete_delay: Duration::ZERO,
        maintain_original_extension: true,
        watch_folder: String::new(),
        follow_symlinks: false,
        can_process_next_item: None,
        on_job_error: Some(Arc::new(move |n, m| {
            e2.lock().unwrap().push((n.into(), m.into()))
        })),
        on_job_complete: Some(Arc::new(move || {
            c2.fetch_add(1, Ordering::SeqCst);
        })),
    };
    tweak(&mut opts);
    let p = Processor::new(opts);
    Env {
        p,
        q,
        client,
        pool,
        src,
        errors,
        completes,
    }
}

fn env() -> Env {
    env_with(false, |_| {})
}

fn write(dir: &Path, rel: &str, size: usize) -> String {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(
        &p,
        (0..size)
            .map(|i| ((i * 31 + 7) % 251) as u8)
            .collect::<Vec<u8>>(),
    )
    .unwrap();
    p.to_string_lossy().into_owned()
}

fn count(e: &Env, table: &str) -> i64 {
    e.q.db()
        .lock()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[tokio::test]
async fn a_file_is_uploaded_completed_linked_and_left_pending_verification() {
    let e = env();
    let path = write(&e.src, "movie.bin", 5000);
    e.q.add_file(&path, 5000).unwrap();
    let c = CancellationToken::new();
    e.p.process_next_item(&c).await.unwrap();
    assert_eq!(
        (
            count(&e, "goqite"),
            count(&e, "in_progress_items"),
            count(&e, "completed_items")
        ),
        (0, 0, 1)
    );
    let (status, nzb): (String, String) =
        e.q.db()
            .lock()
            .query_row(
                "SELECT verification_status, nzb_path FROM completed_items",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
    assert_eq!(
        status, "pending_verification",
        "durable mode defers verification"
    );
    assert!(
        Path::new(&nzb).exists() && nzb.ends_with("movie.bin.nzb"),
        "{nzb}"
    );
    assert_eq!(e.client.posted.load(Ordering::SeqCst), 5);
    assert_eq!(count(&e, "transfer_files"), 1);
    let linked: String =
        e.q.db()
            .lock()
            .query_row("SELECT completed_item_id FROM transfer_files", [], |r| {
                r.get(0)
            })
            .unwrap();
    let id: String =
        e.q.db()
            .lock()
            .query_row("SELECT id FROM completed_items", [], |r| r.get(0))
            .unwrap();
    assert_eq!(linked, id);
    assert_eq!(e.completes.load(Ordering::SeqCst), 1);
    assert!(e.p.durable_mode_enabled() && !e.p.is_path_being_processed(&path));
}

#[tokio::test]
async fn failures_retry_under_new_ids_then_end_up_errored_with_a_callback() {
    let e = env();
    e.client.fail_all.store(true, Ordering::SeqCst);
    let path = write(&e.src, "bad.bin", 3000);
    e.q.add_file(&path, 3000).unwrap();
    let c = CancellationToken::new();
    for attempt in 1..=2 {
        e.p.process_next_item(&c).await.unwrap();
        assert_eq!(
            (
                count(&e, "goqite"),
                count(&e, "in_progress_items"),
                count(&e, "errored_items")
            ),
            (1, 0, 0),
            "attempt {attempt}: re-queued"
        );
        let rc: i64 =
            e.q.db()
                .lock()
                .query_row(
                    "SELECT json_extract(body,'$.retryCount') FROM goqite",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
        assert_eq!(rc, attempt);
    }
    e.p.process_next_item(&c).await.unwrap();
    assert_eq!((count(&e, "goqite"), count(&e, "errored_items")), (0, 1));
    let errs = e.errors.lock().unwrap().clone();
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].0, "bad.bin");
    assert!(errs[0].1.contains("failed"), "{}", errs[0].1);
    assert_eq!(e.completes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancelling_a_running_job_cleans_up_without_an_error_or_retry() {
    let e = env();
    e.client.delay_ms.store(300, Ordering::SeqCst);
    let path = write(&e.src, "slow.bin", 4000);
    e.q.add_file(&path, 4000).unwrap();
    let c = CancellationToken::new();
    let (p2, c2) = (e.p.clone(), c.clone());
    let h = tokio::spawn(async move { p2.process_next_item(&c2).await });
    let id = loop {
        tokio::time::sleep(Duration::from_millis(10)).await;
        if let Some(id) = e.p.get_running_jobs().into_keys().next() {
            break id;
        }
    };
    assert!(e.p.is_path_being_processed(&path));
    let details = e.p.get_running_job_details();
    assert_eq!(
        (details[&id].file_name.as_str(), details[&id].size),
        ("slow.bin", 4000)
    );
    assert_eq!(
        e.p.get_running_job_items(),
        [RunningJobItem { id: id.clone() }]
    );
    e.p.cancel_job(&id).unwrap();
    assert!(
        e.p.cancel_job(&id)
            .unwrap_err()
            .contains("is not currently running")
    );
    h.await.unwrap().unwrap();
    assert_eq!(
        (
            count(&e, "goqite"),
            count(&e, "in_progress_items"),
            count(&e, "errored_items"),
            count(&e, "completed_items")
        ),
        (0, 0, 0, 0)
    );
    assert!(e.p.get_running_jobs().is_empty() && !e.p.is_path_being_processed(&path));
}

#[tokio::test]
async fn cancelling_during_the_delete_delay_keeps_the_originals() {
    // the runtime is built while the pool reports no upload client, so the job runs in legacy
    // (non-durable) mode and the delete delay applies
    let e = env_with(true, |o| {
        o.delete_delay = Duration::from_secs(30);
        o.pool = Some(Arc::new(Pool {
            client: Arc::new(Mock::default()),
            health: Mutex::new(vec![]),
            no_upload_calls: AtomicUsize::new(1),
        }));
    });
    assert!(!e.p.durable_mode_enabled());
    let path = write(&e.src, "keep.bin", 4000);
    e.q.add_file(&path, 4000).unwrap();
    let c = CancellationToken::new();
    let (p2, c2) = (e.p.clone(), c.clone());
    let h = tokio::spawn(async move { p2.process_next_item(&c2).await });
    let id = loop {
        tokio::time::sleep(Duration::from_millis(10)).await;
        if let Some(id) = e.p.get_running_jobs().into_keys().next() {
            break id;
        }
    };
    // the upload is near-instant, so after this the job is waiting out the delete delay
    tokio::time::sleep(Duration::from_millis(500)).await;
    e.p.cancel_job(&id).unwrap();
    tokio::time::timeout(Duration::from_secs(10), h)
        .await
        .expect("the delete delay must end on cancel")
        .unwrap()
        .unwrap();
    assert!(Path::new(&path).exists(), "originals must survive a cancel");
}

#[tokio::test]
async fn an_app_shutdown_keeps_the_in_progress_row_for_crash_recovery() {
    let e = env();
    e.client.delay_ms.store(300, Ordering::SeqCst);
    let path = write(&e.src, "slow.bin", 4000);
    e.q.add_file(&path, 4000).unwrap();
    let c = CancellationToken::new();
    let (p2, c2) = (e.p.clone(), c.clone());
    let h = tokio::spawn(async move { p2.process_next_item(&c2).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    c.cancel();
    h.await.unwrap().unwrap();
    assert_eq!(
        (count(&e, "in_progress_items"), count(&e, "errored_items")),
        (1, 0)
    );
}

#[tokio::test]
async fn pause_blocks_dispatch_and_resume_restores_it() {
    let e = env();
    let path = write(&e.src, "a.bin", 2000);
    e.q.add_file(&path, 2000).unwrap();
    let c = CancellationToken::new();
    e.p.pause_processing();
    assert!(e.p.is_paused());
    e.p.process_queue_items(&c);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(count(&e, "goqite"), 1, "nothing dispatched while paused");
    e.p.resume_processing();
    e.p.process_queue_items(&c);
    for _ in 0..100 {
        if count(&e, "completed_items") == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(count(&e, "completed_items"), 1);
}

#[tokio::test]
async fn the_dispatcher_loop_drains_the_queue_across_slots_and_stops_on_cancel() {
    let e = env();
    for i in 0..5 {
        let p = write(&e.src, &format!("f{i}.bin"), 1500);
        e.q.add_file(&p, 1500).unwrap();
    }
    let c = CancellationToken::new();
    let (p2, c2) = (e.p.clone(), c.clone());
    let h = tokio::spawn(async move { p2.start(&c2).await });
    for _ in 0..150 {
        if count(&e, "completed_items") == 5 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(count(&e, "completed_items"), 5);
    c.cancel();
    h.await.unwrap();
}

#[tokio::test]
async fn min_size_to_start_and_the_callback_hold_back_new_jobs() {
    let e = env_with(false, |o| o.queue_config.min_size_to_start = 10_000);
    let path = write(&e.src, "a.bin", 2000);
    e.q.add_file(&path, 2000).unwrap();
    let c = CancellationToken::new();
    e.p.process_next_item(&c).await.unwrap();
    assert_eq!(count(&e, "goqite"), 1, "below the threshold: held");
    let allow = Arc::new(AtomicBool::new(false));
    let a2 = allow.clone();
    let held = env_with(false, move |o| {
        o.can_process_next_item = Some(Arc::new(move || a2.load(Ordering::SeqCst)))
    });
    let p2 = write(&held.src, "b.bin", 1000);
    held.q.add_file(&p2, 1000).unwrap();
    held.p.process_queue_items(&c);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(count(&held, "goqite"), 1);
    allow.store(true, Ordering::SeqCst);
    held.p.process_queue_items(&c);
    for _ in 0..100 {
        if count(&held, "completed_items") == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(count(&held, "completed_items"), 1);
}

#[tokio::test]
async fn folder_jobs_post_all_files_as_one_nzb() {
    let e = env();
    let root = e.src.join("Show");
    write(&root, "ep1.bin", 3000);
    write(&root, "extras/bonus.bin", 2000);
    // a symlink in the folder is skipped (best effort where symlinks need a privilege)
    let _ = crate::testutil::symlink_file(&root.join("ep1.bin"), &root.join("link.bin"));
    let job_path = format!("FOLDER:{}", root.display());
    e.q.add_file(&job_path, 5000).unwrap();
    e.p.process_next_item(&CancellationToken::new())
        .await
        .unwrap();
    let nzb: String =
        e.q.db()
            .lock()
            .query_row("SELECT nzb_path FROM completed_items", [], |r| r.get(0))
            .unwrap();
    assert!(
        crate::testutil::slash(&nzb).ends_with("Show/Show.nzb"),
        "{nzb}"
    );
    let text = std::fs::read_to_string(nzb).unwrap();
    assert!(
        text.contains("Show/extras/bonus.bin")
            && text.contains("Show/ep1.bin")
            && !text.contains("link.bin"),
        "{text}"
    );
    assert!(
        e.p.collect_files_in_folder(&format!("{}/missing", root.display()))
            .is_err()
    );
    let follow = env_with(false, |o| o.follow_symlinks = true);
    let files = follow
        .p
        .collect_files_in_folder(&root.to_string_lossy())
        .unwrap();
    assert_eq!(
        files
            .iter()
            .map(|f| f.relative_path.as_str())
            .collect::<Vec<_>>(),
        ["Show/ep1.bin", "Show/extras/bonus.bin", "Show/link.bin"]
    );
}

#[tokio::test]
async fn an_empty_folder_job_is_an_error_that_follows_the_retry_path() {
    let e = env();
    let empty = e.src.join("Empty");
    std::fs::create_dir_all(&empty).unwrap();
    e.q.add_file(&format!("FOLDER:{}", empty.display()), 0)
        .unwrap();
    e.p.process_next_item(&CancellationToken::new())
        .await
        .unwrap();
    assert_eq!((count(&e, "goqite"), count(&e, "completed_items")), (1, 0));
}

#[tokio::test]
async fn originals_are_retained_in_durable_mode_even_when_deletion_is_configured() {
    let e = env_with(true, |_| {});
    let path = write(&e.src, "keep.bin", 2000);
    e.q.add_file(&path, 2000).unwrap();
    e.p.process_next_item(&CancellationToken::new())
        .await
        .unwrap();
    assert!(Path::new(&path).exists(), "deleted only after verification");
    let policy: String =
        e.q.db()
            .lock()
            .query_row("SELECT cleanup_policy FROM transfer_files", [], |r| {
                r.get(0)
            })
            .unwrap();
    assert_eq!(
        policy, "delete_original",
        "the intent is persisted for the cleaner"
    );
}

#[test]
fn provider_availability_blocks_and_unblocks_new_jobs_without_touching_manual_pause() {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let e = env();
        let h = |active, errors| {
            vec![ProviderHealth {
                name: "a".into(),
                active_connections: active,
                errors,
            }]
        };
        *e.pool.health.lock().unwrap() = h(0, 5);
        e.p.check_and_handle_provider_availability(); // first sight seeds the baseline
        assert!(!e.p.is_auto_paused());
        *e.pool.health.lock().unwrap() = h(0, 9);
        e.p.check_and_handle_provider_availability();
        assert!(
            e.p.is_auto_paused() && e.p.auto_pause_reason() == "All NNTP providers are unavailable"
        );
        e.p.pause_processing();
        *e.pool.health.lock().unwrap() = h(0, 9);
        e.p.check_and_handle_provider_availability(); // no new errors: unblocked
        assert!(!e.p.is_auto_paused() && e.p.auto_pause_reason().is_empty());
        assert!(e.p.is_paused(), "manual pause stays");
    });
}

#[test]
fn originals_to_delete_adds_only_same_dir_reused_par2() {
    let fi = |p: &str| FileInfo {
        path: p.into(),
        ..Default::default()
    };
    let got = originals_to_delete(
        &[fi("/a/x.mkv"), fi("/a/./x.mkv"), fi("/a/y.mkv")],
        &[
            "/a/x.mkv.par2".into(),
            "/work/x.mkv.par2".into(),
            "/a/y.mkv".into(),
        ],
    );
    assert_eq!(got, ["/a/x.mkv", "/a/y.mkv", "/a/x.mkv.par2"]);
}

#[tokio::test]
async fn close_cancels_running_jobs_and_blocks_new_ones() {
    let e = env();
    e.client.delay_ms.store(5000, Ordering::SeqCst);
    let path = write(&e.src, "slow.bin", 4000);
    e.q.add_file(&path, 4000).unwrap();
    let c = CancellationToken::new();
    let (p2, c2) = (e.p.clone(), c.clone());
    let h = tokio::spawn(async move { p2.process_next_item(&c2).await });
    tokio::time::sleep(Duration::from_millis(100)).await;
    let t = Instant::now();
    e.p.close().await;
    assert!(t.elapsed() < Duration::from_secs(5));
    h.await.unwrap().unwrap();
    e.q.add_file(&write(&e.src, "after.bin", 1000), 1000)
        .unwrap();
    e.p.process_next_item(&CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(count(&e, "goqite"), 1, "a closed processor takes nothing");
}

/// PAR2 on, written to a private work dir, `uploads` upload slots, uploads slowed so a prefetch can run.
fn prefetch_env(uploads: i64, tweak: impl FnOnce(&mut ProcessorOptions)) -> Env {
    let work = tmp();
    let e = env_with(false, |o| {
        let mut c = (*o.config).clone();
        c.par2.enabled = Some(true);
        c.par2.temp_dir = work.to_string_lossy().into_owned();
        c.par2.redundancy = "10%".into();
        o.config = Arc::new(c);
        o.queue_config.max_concurrent_uploads = uploads;
        tweak(o);
    });
    e.client.delay_ms.store(5, Ordering::SeqCst);
    e
}

fn prefetcher(e: &Env) -> Arc<crate::par2::prefetch::Prefetcher> {
    e.p.runtime().unwrap().par2_prefetcher()
}

/// The prefetch is planned off the job's thread, so it is waited for rather than read at once.
async fn held_prefetch(e: &Env) -> Option<String> {
    for _ in 0..200 {
        if let Some(o) = prefetcher(e).owner() {
            return Some(o);
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    None
}

/// Lets any planned prefetch land before asserting that none was made.
async fn settle_prefetch() {
    tokio::time::sleep(Duration::from_millis(200)).await;
}

/// Two queued jobs; the second one's PAR2 is created during the first one's upload and then used.
async fn assert_next_job_par2_is_prefetched(
    wait_for_par2: bool,
    make_jobs: impl FnOnce(&Path) -> [String; 2],
) {
    let e = prefetch_env(1, |o| {
        let mut c = (*o.config).clone();
        c.posting.wait_for_par2 = Some(wait_for_par2);
        o.config = Arc::new(c);
    });
    for j in make_jobs(&e.src) {
        e.q.add_file(&j, 20_000).unwrap();
    }
    let c = CancellationToken::new();
    e.p.process_next_item(&c).await.unwrap();
    let held = held_prefetch(&e)
        .await
        .expect("the second job's PAR2 is prefetched during the first upload");
    assert_eq!(held, e.q.peek_file().unwrap().unwrap().msg_id);
    e.p.process_next_item(&c).await.unwrap();
    assert_eq!(
        prefetcher(&e).hits(),
        1,
        "the second job used the prefetched set"
    );
    assert_eq!(count(&e, "completed_items"), 2);
    assert!(e.errors.lock().unwrap().is_empty());
    assert!(prefetcher(&e).owner().is_none(), "nothing left to prefetch");
}

#[tokio::test]
async fn the_next_jobs_par2_is_created_while_the_current_job_uploads() {
    for wait in [true, false] {
        assert_next_job_par2_is_prefetched(wait, |src| {
            ["a.bin", "b.bin"].map(|n| write(src, n, 20_000))
        })
        .await;
    }
}

#[tokio::test]
async fn a_queued_folder_jobs_set_is_prefetched_with_the_arguments_posting_uses() {
    assert_next_job_par2_is_prefetched(true, |src| {
        ["Show.S01", "Show.S02"].map(|folder| {
            for f in ["e1.mkv", "e2.mkv"] {
                write(src, &format!("{folder}/{f}"), 20_000);
            }
            format!("{FOLDER_PREFIX}{}", src.join(folder).to_string_lossy())
        })
    })
    .await;
}

#[tokio::test]
async fn several_upload_slots_already_overlap_jobs_so_nothing_is_prefetched() {
    let e = prefetch_env(2, |_| {});
    for n in ["a.bin", "b.bin"] {
        let p = write(&e.src, n, 20_000);
        e.q.add_file(&p, 20_000).unwrap();
    }
    e.p.process_next_item(&CancellationToken::new())
        .await
        .unwrap();
    settle_prefetch().await;
    assert!(prefetcher(&e).owner().is_none());
}

#[tokio::test]
async fn a_held_dispatcher_prefetches_nothing() {
    let e = prefetch_env(1, |o| o.can_process_next_item = Some(Arc::new(|| false)));
    for n in ["a.bin", "b.bin"] {
        let p = write(&e.src, n, 20_000);
        e.q.add_file(&p, 20_000).unwrap();
    }
    e.p.process_next_item(&CancellationToken::new())
        .await
        .unwrap();
    settle_prefetch().await;
    assert!(prefetcher(&e).owner().is_none());
}

#[tokio::test]
async fn a_next_job_whose_set_name_is_taken_in_the_work_dir_is_not_prefetched() {
    let e = prefetch_env(1, |_| {});
    // same file name in two folders: both sets would be <work>/x.bin.par2
    let (a, b) = (
        write(&e.src, "one/x.bin", 20_000),
        write(&e.src, "two/x.bin", 30_000),
    );
    e.q.add_file(&a, 20_000).unwrap();
    e.q.add_file(&b, 30_000).unwrap();
    let c = CancellationToken::new();
    e.p.process_next_item(&c).await.unwrap();
    settle_prefetch().await;
    assert!(
        prefetcher(&e).owner().is_none(),
        "job a's set is still in the work dir"
    );
    e.p.process_next_item(&c).await.unwrap();
    assert_eq!(count(&e, "completed_items"), 2);
    assert_eq!(prefetcher(&e).hits(), 0);
}

fn file_name_of_path(path: &str) -> String {
    Path::new(path)
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

#[tokio::test]
async fn a_replaced_processor_hands_its_prefetch_over_to_be_removed_before_the_new_one_starts() {
    let old = prefetch_env(1, |_| {});
    for n in ["a.bin", "b.bin"] {
        let p = write(&old.src, n, 20_000);
        old.q.add_file(&p, 20_000).unwrap();
    }
    old.p
        .process_next_item(&CancellationToken::new())
        .await
        .unwrap();
    held_prefetch(&old).await.expect("a prefetch is held");
    let work = PathBuf::from(old.p.runtime().unwrap().par2_work_dir());
    let next = file_name_of_path(&old.q.peek_file().unwrap().unwrap().job.path);
    let handed = old
        .p
        .stop_par2_prefetch()
        .expect("the held prefetch is handed over");
    assert!(prefetcher(&old).owner().is_none());
    // a stopped processor starts no new prefetch
    if let Some(h) = old.p.spawn_prefetch_next() {
        h.await.unwrap();
    }
    assert!(prefetcher(&old).owner().is_none());

    let new = env();
    new.p.inherit_par2_discard(handed);
    let c = CancellationToken::new();
    c.cancel();
    new.p.start(&c).await;
    let left: Vec<_> = std::fs::read_dir(&work)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .filter(|n| n.starts_with(&next))
        .collect();
    assert!(
        left.is_empty(),
        "removed before the new processor's first dispatch: {left:?}"
    );
}

#[tokio::test]
async fn a_job_without_its_own_par2_still_starts_the_next_jobs_prefetch() {
    let e = prefetch_env(1, |o| {
        let mut c = (*o.config).clone();
        c.par2.skip_if_par2_exists = Some(true);
        o.config = Arc::new(c);
    });
    // first: a folder that ships its own PAR2, so its job makes no PAR2 call
    write(&e.src, "Shipped/data.bin", 20_000);
    write(&e.src, "Shipped/data.bin.par2", 100);
    let folder = format!("{FOLDER_PREFIX}{}", e.src.join("Shipped").to_string_lossy());
    e.q.add_file_with_priority(&folder, 20_100, 5).unwrap();
    let next = write(&e.src, "next.bin", 20_000);
    e.q.add_file(&next, 20_000).unwrap();
    let c = CancellationToken::new();
    e.p.process_next_item(&c).await.unwrap();
    held_prefetch(&e)
        .await
        .expect("the next job's PAR2 is prefetched during the folder upload");
    e.p.process_next_item(&c).await.unwrap();
    assert_eq!(prefetcher(&e).hits(), 1);
    assert_eq!(count(&e, "completed_items"), 2);
}

#[test]
fn a_prefetch_needs_room_for_the_extra_set_twice_over_plus_a_reserve() {
    const GIB: u64 = 1 << 30;
    // 10 GiB of input at 10% is a ~1 GiB set: needs 2 GiB + 1 GiB reserve
    let est = prefetch_set_estimate(10 * GIB, "10%");
    assert!((GIB..GIB + GIB / 10).contains(&est), "{est}");
    assert!(prefetch_fits(Some(4 * GIB), est));
    assert!(!prefetch_fits(Some(2 * GIB), est));
    assert!(
        prefetch_fits(None, est),
        "unknown free space does not block it"
    );
}
