use std::collections::HashSet;
use std::sync::atomic::AtomicUsize;

use postie_nntp::NntpError;

use super::runtime::RuntimeDeps;
use super::*;
use crate::config::{CustomHeader, Duration as D, NewsgroupConfig, PostHeaders};
use crate::db::Db;
use crate::poster::BoxFut;
use crate::transferstore::Store;

#[derive(Default)]
struct Mock {
    posts: Mutex<Vec<Vec<u8>>>,
    fail_all: AtomicBool,
    posted: AtomicUsize,
}

impl ArticleClient for Mock {
    fn post(&self, article: Vec<u8>) -> BoxFut<'_, Result<(), NntpError>> {
        Box::pin(async move {
            if self.fail_all.load(Ordering::SeqCst) {
                return Err(NntpError::Closed);
            }
            self.posted.fetch_add(1, Ordering::SeqCst);
            self.posts.lock().unwrap().push(article);
            Ok(())
        })
    }
    fn stat<'a>(&'a self, _: &'a str) -> BoxFut<'a, Result<bool, NntpError>> {
        Box::pin(async { Ok(true) })
    }
    fn max_connections(&self) -> usize {
        4
    }
}

fn tmp() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "postie-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn file(dir: &Path, rel: &str, size: usize) -> FileInfo {
    let p = dir.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(
        &p,
        (0..size)
            .map(|i| ((i * 31 + 7) % 251) as u8)
            .collect::<Vec<u8>>(),
    )
    .unwrap();
    FileInfo {
        path: p.to_string_lossy().into_owned(),
        size: size as u64,
        relative_path: rel.into(),
    }
}

fn cfg(par2: bool, wait: bool) -> ConfigData {
    let mut c = ConfigData::default();
    c.posting.article_size_in_bytes = 1000;
    c.posting.max_retries = 3;
    c.posting.wait_for_par2 = Some(wait);
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
    c.par2.enabled = Some(par2);
    c.par2.redundancy = "10".into();
    c.par2.gf16_method = "auto".into();
    c.par2.num_goroutines = 2;
    c.par2.maintain_par2_files = Some(false);
    c.par2.skip_if_par2_exists = Some(true);
    c.maintain_original_extension = Some(true);
    c
}

fn postie(c: &ConfigData, mock: &Arc<Mock>, rt: Option<Arc<Runtime>>, transfer: &str) -> Postie {
    Postie::new(
        c,
        mock.clone(),
        mock.clone(),
        Arc::new(JobProgress::new("j")),
        None,
        rt,
        transfer,
        None,
    )
}

fn nzb_text(p: &str) -> String {
    std::fs::read_to_string(p).unwrap()
}

/// A PAR2 executor double: fails immediately, or blocks until its cancel token fires.
struct FakePar2 {
    block_until_cancelled: bool,
    saw_cancel: AtomicBool,
    calls: AtomicUsize,
}

impl FakePar2 {
    fn new(block_until_cancelled: bool) -> Arc<Self> {
        Arc::new(Self {
            block_until_cancelled,
            saw_cancel: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
        })
    }

    fn run<'a>(
        &'a self,
        cancel: &'a CancellationToken,
    ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if !self.block_until_cancelled {
                return Err(ExecError::Msg("par2 exploded".into()));
            }
            tokio::time::timeout(std::time::Duration::from_secs(30), cancel.cancelled())
                .await
                .map_err(|_| ExecError::Msg("never cancelled".into()))?;
            self.saw_cancel.store(true, Ordering::SeqCst);
            Err(ExecError::Cancelled)
        })
    }
}

impl Par2Executor for FakePar2 {
    fn create<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        _: &'a [FileInfo],
    ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
        self.run(cancel)
    }
    fn create_in_directory<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        _: &'a [FileInfo],
        _: &'a str,
    ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
        self.run(cancel)
    }
    fn create_set<'a>(
        &'a self,
        cancel: &'a CancellationToken,
        _: &'a [FileInfo],
        _: &'a str,
        _: &'a str,
        _: &'a str,
    ) -> BoxFut<'a, Result<Par2Result, ExecError>> {
        self.run(cancel)
    }
}

#[tokio::test]
async fn one_nzb_per_file_with_par2_posted_and_cleaned_up_in_standalone_mode() {
    let (src, out) = (tmp(), tmp());
    let f = file(&src, "movie.bin", 20_000);
    for wait in [true, false] {
        let mock = Arc::new(Mock::default());
        let p = postie(&cfg(true, wait), &mock, None, "");
        let o = p
            .post(
                &CancellationToken::new(),
                std::slice::from_ref(&f),
                src.to_str().unwrap(),
                out.to_str().unwrap(),
                false,
            )
            .await
            .unwrap();
        assert!(
            o.deferred.is_none() && o.nzb_path.ends_with("movie.bin.nzb"),
            "{}",
            o.nzb_path
        );
        let nzb = nzb_text(&o.nzb_path);
        assert!(
            nzb.contains("movie.bin.par2") && nzb.contains("movie.bin.vol"),
            "wait={wait}: par2 files are in the NZB"
        );
        assert!(
            mock.posted.load(Ordering::SeqCst) >= 20 + 2,
            "main + par2 articles"
        );
        assert!(
            std::fs::read_dir(&src)
                .unwrap()
                .flatten()
                .all(|e| !e.file_name().to_string_lossy().ends_with(".par2")),
            "generated PAR2 removed after a successful standalone post"
        );
        assert!(p.reused_par2_files().is_empty());
        p.close();
    }
}

#[tokio::test]
async fn maintain_par2_writes_them_to_the_output_dir_and_keeps_them() {
    let (src, out) = (tmp(), tmp());
    let f = file(&src, "sub/a.bin", 8_000);
    let mut c = cfg(true, true);
    c.par2.maintain_par2_files = Some(true);
    let mock = Arc::new(Mock::default());
    let p = postie(&c, &mock, None, "");
    let o = p
        .post(
            &CancellationToken::new(),
            &[f],
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            false,
        )
        .await
        .unwrap();
    assert!(
        o.nzb_path.replace('\\', "/").contains("/sub/a.bin.nzb"),
        "{}",
        o.nzb_path
    );
    assert!(out.join("sub/a.bin.par2").exists());
}

#[tokio::test]
async fn par2_creation_failure_does_not_fail_the_upload_in_any_mode() {
    let (src, out) = (tmp(), tmp());
    let f = file(&src, "a.bin", 5_000);
    for wait in [true, false] {
        let mock = Arc::new(Mock::default());
        let mut p = postie(&cfg(true, wait), &mock, None, "");
        let fake = FakePar2::new(false);
        p.par2 = fake.clone();
        let o = p
            .post(
                &CancellationToken::new(),
                std::slice::from_ref(&f),
                src.to_str().unwrap(),
                out.to_str().unwrap(),
                false,
            )
            .await
            .unwrap();
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1, "wait={wait}");
        let nzb = nzb_text(&o.nzb_path);
        assert!(nzb.contains("a.bin"), "wait={wait}");
        assert!(!nzb.contains(".par2"), "wait={wait}: only the source file");
        p.close();
    }
}

#[tokio::test]
async fn par2_disabled_posts_only_the_file() {
    let (src, out) = (tmp(), tmp());
    let f = file(&src, "a.bin", 5_000);
    let mock = Arc::new(Mock::default());
    let p = postie(&cfg(false, true), &mock, None, "");
    let o = p
        .post(
            &CancellationToken::new(),
            &[f],
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            false,
        )
        .await
        .unwrap();
    assert_eq!(mock.posted.load(Ordering::SeqCst), 5);
    assert!(!nzb_text(&o.nzb_path).contains(".par2"));
}

#[tokio::test]
async fn a_failing_upload_keeps_par2_files_for_the_retry_and_writes_no_nzb() {
    let (src, out) = (tmp(), tmp());
    let f = file(&src, "a.bin", 5_000);
    let mock = Arc::new(Mock::default());
    mock.fail_all.store(true, Ordering::SeqCst);
    let p = postie(&cfg(true, true), &mock, None, "");
    let e = p
        .post(
            &CancellationToken::new(),
            &[f],
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            false,
        )
        .await
        .unwrap_err();
    assert!(matches!(e, PostieError::Post(_)), "{e}");
    assert!(src.join("a.bin.par2").exists(), "kept for the retry");
    assert_eq!(std::fs::read_dir(&out).unwrap().count(), 0);
}

#[tokio::test]
async fn a_folder_is_one_nzb_with_a_single_par2_set_and_relative_subjects() {
    let (root, out) = (tmp(), tmp());
    let a = file(&root, "Show/ep1.bin", 6_000);
    let b = file(&root, "Show/extras/bonus.bin", 4_000);
    let mock = Arc::new(Mock::default());
    let p = postie(&cfg(true, true), &mock, None, "");
    let o = p
        .post(
            &CancellationToken::new(),
            &[a, b],
            root.to_str().unwrap(),
            out.to_str().unwrap(),
            true,
        )
        .await
        .unwrap();
    assert!(
        o.nzb_path.replace('\\', "/").ends_with("Show/Show.nzb"),
        "{}",
        o.nzb_path
    );
    let nzb = nzb_text(&o.nzb_path);
    assert!(
        nzb.contains("Show.par2") && nzb.contains("extras/bonus.bin"),
        "{nzb}"
    );
    // the PAR2 set was generated next to the first input and removed after a successful standalone post
    assert!(!root.join("Show/Show.par2").exists());
}

#[tokio::test]
async fn a_folder_with_existing_par2_skips_generation() {
    let (root, out) = (tmp(), tmp());
    let a = file(&root, "Show/ep1.bin", 6_000);
    let existing = file(&root, "Show/ep1.bin.par2", 300);
    let mock = Arc::new(Mock::default());
    let p = postie(&cfg(true, true), &mock, None, "");
    let o = p
        .post(
            &CancellationToken::new(),
            &[a, existing],
            root.to_str().unwrap(),
            out.to_str().unwrap(),
            true,
        )
        .await
        .unwrap();
    assert!(nzb_text(&o.nzb_path).contains("ep1.bin.par2"));
    assert!(!root.join("Show/Show.par2").exists());
}

#[tokio::test]
async fn parallel_folder_mode_also_produces_the_nzb() {
    let (root, out) = (tmp(), tmp());
    let a = file(&root, "Show/ep1.bin", 6_000);
    let mock = Arc::new(Mock::default());
    let p = postie(&cfg(true, false), &mock, None, "");
    let o = p
        .post(
            &CancellationToken::new(),
            &[a],
            root.to_str().unwrap(),
            out.to_str().unwrap(),
            true,
        )
        .await
        .unwrap();
    assert!(nzb_text(&o.nzb_path).contains("Show.par2"));
}

#[tokio::test]
async fn a_failed_main_upload_cancels_the_parallel_par2_branch() {
    let (src, out) = (tmp(), tmp());
    let f = file(&src, "Show/a.bin", 5_000);
    for folder in [false, true] {
        let mock = Arc::new(Mock::default());
        mock.fail_all.store(true, Ordering::SeqCst);
        let mut p = postie(&cfg(true, false), &mock, None, "");
        let fake = FakePar2::new(true);
        p.par2 = fake.clone();
        let outer = CancellationToken::new();
        let e = p
            .post(
                &outer,
                std::slice::from_ref(&f),
                src.to_str().unwrap(),
                out.to_str().unwrap(),
                folder,
            )
            .await
            .unwrap_err();
        assert!(matches!(e, PostieError::Post(_)), "folder={folder}: {e}");
        assert_eq!(fake.calls.load(Ordering::SeqCst), 1, "folder={folder}");
        assert!(
            fake.saw_cancel.load(Ordering::SeqCst),
            "folder={folder}: PAR2 observed the cancellation"
        );
        assert!(!outer.is_cancelled(), "the caller's token is untouched");
        assert_eq!(mock.posted.load(Ordering::SeqCst), 0);
        p.close();
    }
}

#[tokio::test]
async fn cancellation_is_reported_as_canceled() {
    let (src, out) = (tmp(), tmp());
    let f = file(&src, "a.bin", 5_000);
    let c = CancellationToken::new();
    c.cancel();
    let mock = Arc::new(Mock::default());
    let e = postie(&cfg(false, true), &mock, None, "")
        .post(
            &c,
            &[f],
            src.to_str().unwrap(),
            out.to_str().unwrap(),
            false,
        )
        .await
        .unwrap_err();
    assert!(matches!(e, PostieError::Canceled), "{e}");
    assert!(
        matches!(postie(&cfg(false, true), &mock, None, "").post(&CancellationToken::new(), &[], "/", "/", false).await, Err(PostieError::Msg(m)) if m == "no files to post")
    );
}

fn runtime_with_store(c: &ConfigData, mock: &Arc<Mock>) -> (Arc<Runtime>, Store, PathBuf) {
    let db = Db::in_memory().unwrap();
    db.lock().execute_batch("PRAGMA foreign_keys = ON").unwrap();
    crate::database::MigrationRunner::new(db.clone())
        .migrate_up()
        .unwrap();
    let store = Store::new(db);
    let dir = tmp();
    let rt = Runtime::new(
        c,
        RuntimeDeps {
            upload: Some(mock.clone()),
            verify: None,
            store: Some(store.clone()),
            manifest_dir: dir.clone(),
            script_queue: None,
        },
    );
    (rt, store, dir)
}

#[tokio::test]
async fn durable_mode_records_manifests_marks_files_uploaded_and_keeps_generated_par2() {
    let (src, out) = (tmp(), tmp());
    let f = file(&src, "movie.bin", 12_000);
    let mut c = cfg(true, true);
    c.post_check.retry_delay = D::new("30s");
    let mock = Arc::new(Mock::default());
    let (rt, store, manifests) = runtime_with_store(&c, &mock);
    assert!(rt.durable_verification_enabled() && rt.upload_engine().is_some());
    let p = postie(&c, &mock, Some(rt.clone()), "t-1");
    p.set_delete_original(true);
    p.post(
        &CancellationToken::new(),
        std::slice::from_ref(&f),
        src.to_str().unwrap(),
        out.to_str().unwrap(),
        false,
    )
    .await
    .unwrap();
    let files = store.list_files_by_transfer("t-1").unwrap();
    assert!(
        files.len() >= 3,
        "original + par2 main + volumes recorded: {}",
        files.len()
    );
    assert!(
        files
            .iter()
            .all(|x| x.upload_state == "uploaded" && x.next_check_at.is_some())
    );
    let orig = files.iter().find(|x| x.file_role == "original").unwrap();
    assert_eq!(
        (orig.cleanup_policy.as_str(), orig.source_path.as_str()),
        ("delete_original", f.path.as_str())
    );
    assert!(files.iter().any(|x| x.file_role == "generated_par2"));
    assert!(
        src.join("movie.bin.par2").exists(),
        "durable mode keeps generated PAR2 for background verification"
    );
    assert!(manifests.join("t-1").exists());
    assert!(
        !rt.par2_in_use().is_empty(),
        "reserved until the job closes"
    );
    p.close();
    assert!(rt.par2_in_use().is_empty());
    rt.discard_transfer("t-1").unwrap();
    assert!(
        store.list_files_by_transfer("t-1").unwrap().is_empty() && !manifests.join("t-1").exists()
    );
}

#[tokio::test]
async fn cleanup_par2_files_removes_the_set_for_a_source() {
    let src = tmp();
    let f = file(&src, "a.bin", 100);
    for n in [
        "a.bin.par2",
        "a.bin.vol0+1.par2",
        "a.bin.vol1+2.par2",
        "other.par2",
    ] {
        std::fs::write(src.join(n), b"x").unwrap();
    }
    let mock = Arc::new(Mock::default());
    postie(&cfg(true, true), &mock, None, "")
        .cleanup_par2_files(&CancellationToken::new(), &f)
        .await;
    let left: HashSet<String> = std::fs::read_dir(&src)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        left,
        HashSet::from(["a.bin".to_string(), "other.par2".to_string()])
    );
}

#[test]
fn runtime_reservations_are_reference_counted_and_metrics_serialise_like_go() {
    let mock = Arc::new(Mock::default());
    let rt = Runtime::new(
        &cfg(true, true),
        RuntimeDeps {
            upload: Some(mock),
            verify: None,
            store: None,
            manifest_dir: PathBuf::new(),
            script_queue: None,
        },
    );
    rt.reserve_par2(&["/a".into(), "/b".into()]);
    rt.reserve_par2(&["/a".into()]);
    rt.release_par2(&["/a".into()]);
    let mut left = rt.par2_in_use();
    left.sort();
    assert_eq!(left, ["/a", "/b"]);
    rt.release_par2(&["/a".into(), "/b".into(), "/never".into()]);
    assert!(rt.par2_in_use().is_empty());
    assert!(
        rt.new_manifest_recorder("t").is_none(),
        "no store, no recorder"
    );
    assert!(!rt.durable_verification_enabled());
    let m = serde_json::to_value(rt.metrics()).unwrap();
    assert!(
        m.get("uploadWorkerCount").is_some() && m["par2Capacity"] == 1,
        "{m}"
    );
}
