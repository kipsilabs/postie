use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::sync::atomic::AtomicI64;

use super::*;
use crate::config::{CustomHeader, NewsgroupConfig, PostHeaders};

struct Tmp(PathBuf);
impl Tmp {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!(
            "poster-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        std::fs::create_dir_all(&p).unwrap();
        Self(p)
    }
    fn file(&self, name: &str, size: usize) -> PathBuf {
        let p = self.0.join(name);
        let data: Vec<u8> = (0..size).map(|i| ((i * 31 + 7) % 251) as u8).collect();
        File::create(&p).unwrap().write_all(&data).unwrap();
        p
    }
}
impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Scripted NNTP client.
#[derive(Default)]
struct Mock {
    posts: Mutex<Vec<Vec<u8>>>,
    present: Mutex<HashSet<String>>,
    /// Treat everything as present unless listed in `missing_forever`.
    all_present: AtomicBool,
    missing_forever: Mutex<HashSet<String>>,
    fail_first: AtomicUsize,
    fail_with: Mutex<Option<NntpError>>,
    stat_calls: AtomicUsize,
    cur: AtomicI64,
    max_cur: AtomicI64,
    delay_ms: AtomicU64,
    conns: usize,
    log: Mutex<Vec<String>>,
}

impl Mock {
    fn new(conns: usize) -> Arc<Self> {
        Arc::new(Self {
            conns,
            ..Default::default()
        })
    }
    fn posted_ids(&self) -> Vec<String> {
        self.posts
            .lock()
            .unwrap()
            .iter()
            .map(|w| header(w, "Message-ID").trim_matches(['<', '>']).to_string())
            .collect()
    }
}

fn header(wire: &[u8], name: &str) -> String {
    let text = String::from_utf8_lossy(wire);
    text.lines()
        .find_map(|l| l.strip_prefix(&format!("{name}: ")).map(str::to_string))
        .unwrap_or_default()
}

/// Decodes the yEnc part of a wire article.
fn decode_body(wire: &[u8]) -> Vec<u8> {
    let s = wire.windows(7).position(|w| w == b"=ypart ").unwrap();
    let start = s + wire[s..].windows(2).position(|w| w == b"\r\n").unwrap() + 2;
    let end = start
        + wire[start..]
            .windows(7)
            .position(|w| w == b"\r\n=yend")
            .unwrap();
    let mut out = Vec::new();
    let mut i = start;
    while i < end {
        match wire[i] {
            b'\r' | b'\n' => {}
            b'=' => {
                i += 1;
                out.push(wire[i].wrapping_sub(64).wrapping_sub(42));
            }
            c => out.push(c.wrapping_sub(42)),
        }
        i += 1;
    }
    out
}

impl ArticleClient for Mock {
    fn post(&self, article: Vec<u8>) -> BoxFut<'_, Result<(), NntpError>> {
        Box::pin(async move {
            let n = self.cur.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_cur.fetch_max(n, Ordering::SeqCst);
            let d = self.delay_ms.load(Ordering::Relaxed);
            if d > 0 {
                tokio::time::sleep(Duration::from_millis(d)).await;
            }
            self.cur.fetch_sub(1, Ordering::SeqCst);
            if self.fail_first.load(Ordering::SeqCst) > 0 {
                self.fail_first.fetch_sub(1, Ordering::SeqCst);
                return Err(self
                    .fail_with
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or(NntpError::Disconnected));
            }
            self.log.lock().unwrap().push("post".into());
            let id = header(&article, "Message-ID")
                .trim_matches(['<', '>'])
                .to_string();
            self.present.lock().unwrap().insert(id);
            self.posts.lock().unwrap().push(article);
            Ok(())
        })
    }
    fn stat<'a>(&'a self, id: &'a str) -> BoxFut<'a, Result<bool, NntpError>> {
        Box::pin(async move {
            self.stat_calls.fetch_add(1, Ordering::SeqCst);
            if self.missing_forever.lock().unwrap().contains(id) {
                return Ok(false);
            }
            Ok(
                self.all_present.load(Ordering::SeqCst)
                    || self.present.lock().unwrap().contains(id),
            )
        })
    }
    fn max_connections(&self) -> usize {
        self.conns
    }
}

struct Sink {
    log: Arc<Mutex<Vec<String>>>,
    existing: Mutex<Option<Vec<ArticleRecord>>>,
    fail_record: bool,
    recorded: Mutex<Vec<Vec<Article>>>,
}
impl Sink {
    fn new(log: Arc<Mutex<Vec<String>>>) -> Self {
        Self {
            log,
            existing: Mutex::new(None),
            fail_record: false,
            recorded: Mutex::default(),
        }
    }
}
impl ManifestSink for Sink {
    fn record_file<'a>(
        &'a self,
        _p: &'a Path,
        articles: &'a [Article],
    ) -> BoxFut<'a, Result<(), String>> {
        Box::pin(async move {
            if self.fail_record {
                return Err("disk full".into());
            }
            self.log.lock().unwrap().push("record".into());
            self.recorded.lock().unwrap().push(articles.to_vec());
            Ok(())
        })
    }
    fn existing_articles<'a>(
        &'a self,
        _p: &'a Path,
    ) -> BoxFut<'a, Result<Option<Vec<ArticleRecord>>, String>> {
        Box::pin(async move { Ok(self.existing.lock().unwrap().clone()) })
    }
}

fn posting() -> PostingConfig {
    PostingConfig {
        article_size_in_bytes: 1000,
        max_retries: 3,
        groups: Some(vec![NewsgroupConfig {
            name: "alt.test".into(),
            enabled: Some(true),
        }]),
        message_id_format: "random".into(),
        obfuscation_policy: "none".into(),
        par2_obfuscation_policy: "none".into(),
        group_policy: "all".into(),
        post_headers: PostHeaders {
            add_nxg_header: false,
            default_from: "T <t@example.com>".into(),
            custom_headers: Some(vec![CustomHeader {
                name: "X-T".into(),
                value: "1".into(),
            }]),
        },
        ..Default::default()
    }
}

fn check_off() -> PostCheck {
    PostCheck {
        enabled: Some(false),
        ..Default::default()
    }
}

fn poster(
    mock: &Arc<Mock>,
    check: PostCheck,
    engine: Option<Arc<Engine>>,
    sink: Option<Arc<dyn ManifestSink>>,
) -> Poster {
    let client: Arc<dyn ArticleClient> = mock.clone();
    Poster::new(
        posting(),
        check,
        client.clone(),
        client,
        Arc::new(JobProgress::new("job")),
        engine,
        sink,
    )
}

fn generator() -> Arc<Generator> {
    Arc::new(Generator::new(1000, Default::default(), true))
}

#[tokio::test]
async fn posts_every_segment_with_correct_bodies_and_fills_the_nzb() {
    let t = Tmp::new();
    let (a, b) = (t.file("a.bin", 2500), t.file("b.bin", 600));
    let mock = Mock::new(4);
    let p = poster(&mock, check_off(), None, None);
    let nzb = generator();
    p.post(
        &CancellationToken::new(),
        &[a.clone(), b.clone()],
        &nzb,
        None,
    )
    .await
    .unwrap();

    let posts = mock.posts.lock().unwrap().clone();
    assert_eq!(posts.len(), 4);
    // every body decodes to its slice of the source file
    let mut got: Vec<Vec<u8>> = posts.iter().map(|w| decode_body(w)).collect();
    got.sort();
    let src_a = std::fs::read(&a).unwrap();
    let src_b = std::fs::read(&b).unwrap();
    let mut want = vec![
        src_a[..1000].to_vec(),
        src_a[1000..2000].to_vec(),
        src_a[2000..].to_vec(),
        src_b.clone(),
    ];
    want.sort();
    assert_eq!(got, want);
    let w = &posts[0];
    assert!(
        header(w, "Newsgroups") == "alt.test"
            && header(w, "From") == "T <t@example.com>"
            && header(w, "X-T") == "1"
    );
    let xml = nzb.render().unwrap();
    assert_eq!(xml.matches("<file ").count(), 2);
    assert_eq!(xml.matches("<segment ").count(), 4);
    let s = p.stats();
    assert_eq!((s.articles_posted, s.bytes_posted), (4, 3100));
}

#[tokio::test]
async fn empty_files_are_skipped_without_hanging() {
    let t = Tmp::new();
    let mock = Mock::new(2);
    let p = poster(&mock, check_off(), None, None);
    let nzb = generator();
    let r = tokio::time::timeout(
        Duration::from_secs(5),
        p.post(
            &CancellationToken::new(),
            &[t.file("empty", 0), t.file("real", 10)],
            &nzb,
            None,
        ),
    )
    .await
    .expect("must not hang");
    r.unwrap();
    assert_eq!(mock.posts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn missing_file_reports_an_add_error() {
    let t = Tmp::new();
    let p = poster(&Mock::new(2), check_off(), None, None);
    let e = p
        .post(
            &CancellationToken::new(),
            &[t.0.join("nope.bin")],
            &generator(),
            None,
        )
        .await
        .unwrap_err();
    assert!(
        matches!(&e, PostError::Add { file, cause } if file.ends_with("nope.bin") && cause.starts_with("error opening file")),
        "{e:?}"
    );
}

#[tokio::test]
async fn a_rejected_article_fails_the_post_with_the_go_message() {
    let t = Tmp::new();
    let mock = Mock::new(2);
    mock.fail_first.store(1000, Ordering::SeqCst);
    *mock.fail_with.lock().unwrap() = Some(NntpError::PostRejected {
        code: 441,
        text: "no".into(),
    });
    let p = poster(&mock, check_off(), None, None);
    let e = p
        .post(
            &CancellationToken::new(),
            &[t.file("a.bin", 1500), t.file("b.bin", 100)],
            &generator(),
            None,
        )
        .await
        .unwrap_err();
    let msg = e.to_string();
    assert!(
        msg.starts_with("failed to post file ")
            && msg.contains("after 3 retries")
            && msg.contains("post rejected: 441 no"),
        "{msg}"
    );
    assert_eq!(p.stats().articles_posted, 0);
}

#[tokio::test]
async fn stale_connection_errors_are_retried_up_to_three_times() {
    let t = Tmp::new();
    let mock = Mock::new(1);
    mock.fail_first.store(2, Ordering::SeqCst);
    let p = poster(&mock, check_off(), None, None);
    p.post(
        &CancellationToken::new(),
        &[t.file("a.bin", 500)],
        &generator(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(mock.posts.lock().unwrap().len(), 1);
    assert_eq!(p.stats().articles_posted, 1);

    let mock = Mock::new(1);
    mock.fail_first.store(3, Ordering::SeqCst);
    let p = poster(&mock, check_off(), None, None);
    assert!(
        p.post(
            &CancellationToken::new(),
            &[t.file("b.bin", 500)],
            &generator(),
            None
        )
        .await
        .is_err(),
        "three stale failures exhaust the attempts"
    );
}

#[tokio::test]
async fn cancellation_returns_canceled() {
    let t = Tmp::new();
    let mock = Mock::new(1);
    mock.delay_ms.store(200, Ordering::Relaxed);
    let p = poster(&mock, check_off(), None, None);
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        c2.cancel();
    });
    let r = tokio::time::timeout(
        Duration::from_secs(5),
        p.post(&cancel, &[t.file("a.bin", 5000)], &generator(), None),
    )
    .await
    .expect("cancel must end the post");
    assert_eq!(r.unwrap_err(), PostError::Canceled);
}

#[tokio::test]
async fn a_closed_poster_refuses_work() {
    let t = Tmp::new();
    let p = poster(&Mock::new(1), check_off(), None, None);
    p.close();
    assert_eq!(
        p.post(
            &CancellationToken::new(),
            &[t.file("a", 10)],
            &generator(),
            None
        )
        .await
        .unwrap_err(),
        PostError::Closed
    );
}

#[tokio::test]
async fn the_engine_bounds_concurrent_posts() {
    let t = Tmp::new();
    let mock = Mock::new(10);
    mock.delay_ms.store(30, Ordering::Relaxed);
    let engine = Arc::new(Engine::new(1000, 0, 2)); // 2 worker slots
    let p = poster(&mock, check_off(), Some(engine.clone()), None);
    p.post(
        &CancellationToken::new(),
        &[t.file("a.bin", 12_000)],
        &generator(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(mock.posts.lock().unwrap().len(), 12);
    assert!(
        mock.max_cur.load(Ordering::SeqCst) <= 2,
        "max concurrent posts {}",
        mock.max_cur.load(Ordering::SeqCst)
    );
    assert_eq!(engine.metrics().reserved_bytes, 0, "buffers are released");
    assert_eq!(engine.metrics().upload_bytes, 12_000);
}

#[tokio::test]
async fn pause_blocks_posting_until_resumed() {
    let t = Tmp::new();
    let mock = Mock::new(2);
    let p = poster(&mock, check_off(), None, None);
    let cancel = CancellationToken::new();
    let gate = PauseGate::new(cancel.clone());
    gate.pause();
    let g2 = gate.clone();
    let file = t.file("a.bin", 500);
    let task = tokio::spawn({
        let (p, cancel) = (p.clone(), cancel.clone());
        async move { p.post(&cancel, &[file], &generator(), Some(&g2)).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        mock.posts.lock().unwrap().is_empty(),
        "nothing is posted while paused"
    );
    gate.resume();
    tokio::time::timeout(Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(mock.posts.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn durable_mode_records_the_manifest_before_any_post_and_skips_the_check() {
    let t = Tmp::new();
    let mock = Mock::new(2);
    let sink = Arc::new(Sink::new(Arc::new(Mutex::new(vec![]))));
    let sink_log = sink.log.clone();
    let dyn_sink: Arc<dyn ManifestSink> = sink.clone();
    // share one log between the sink and the mock to prove the order
    let p = Poster::new(
        posting(),
        PostCheck {
            enabled: Some(true),
            ..Default::default()
        },
        mock.clone(),
        mock.clone(),
        Arc::new(JobProgress::new("j")),
        None,
        Some(dyn_sink),
    );
    p.post(
        &CancellationToken::new(),
        &[t.file("a.bin", 1500)],
        &generator(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(sink_log.lock().unwrap().as_slice(), ["record"]);
    assert_eq!(sink.recorded.lock().unwrap()[0].len(), 2);
    assert_eq!(
        mock.stat_calls.load(Ordering::SeqCst),
        0,
        "no in-poster check in durable mode"
    );
    // the manifest holds exactly the Message-IDs that were posted
    let mut recorded: Vec<String> = sink.recorded.lock().unwrap()[0]
        .iter()
        .map(|a| a.message_id.clone())
        .collect();
    let mut posted = mock.posted_ids();
    recorded.sort();
    posted.sort();
    assert_eq!(recorded, posted);
}

#[tokio::test]
async fn a_failing_sink_aborts_before_posting() {
    let t = Tmp::new();
    let mock = Mock::new(2);
    let mut sink = Sink::new(Arc::new(Mutex::new(vec![])));
    sink.fail_record = true;
    let p = Poster::new(
        posting(),
        check_off(),
        mock.clone(),
        mock.clone(),
        Arc::new(JobProgress::new("j")),
        None,
        Some(Arc::new(sink)),
    );
    let e = p
        .post(
            &CancellationToken::new(),
            &[t.file("a.bin", 1500)],
            &generator(),
            None,
        )
        .await
        .unwrap_err();
    assert!(
        e.to_string().contains("recording manifest for") && e.to_string().contains("disk full"),
        "{e}"
    );
    assert!(mock.posts.lock().unwrap().is_empty());
}

#[tokio::test]
async fn recovery_reposts_only_missing_articles_but_keeps_all_in_the_nzb() {
    let t = Tmp::new();
    let file = t.file("a.bin", 2500);
    let mock = Mock::new(2);
    // a manifest from an interrupted upload: three articles, the first two already on the server
    let recs: Vec<ArticleRecord> = (0..3)
        .map(|i| ArticleRecord {
            index: i,
            source_path: file.to_string_lossy().into_owned(),
            file_role: "original".into(),
            offset: i * 1000,
            body_size: if i == 2 { 500 } else { 1000 },
            message_id: format!("old{i}@x"),
            subject: "s".into(),
            original_subject: "os".into(),
            from: "f <f@x.y>".into(),
            groups: vec!["alt.test".into()],
            date: chrono::DateTime::parse_from_rfc3339("2023-11-14T22:13:20Z").unwrap(),
            custom_headers: BTreeMap::new(),
            xnxg_header: String::new(),
            file_name: "a.bin".into(),
            part_number: i + 1,
            total_parts: 3,
            file_size: 2500,
        })
        .collect();
    mock.present
        .lock()
        .unwrap()
        .extend(["old0@x".to_string(), "old1@x".to_string()]);
    let sink = Sink::new(Arc::new(Mutex::new(vec![])));
    *sink.existing.lock().unwrap() = Some(recs);
    let sink = Arc::new(sink);
    let p = Poster::new(
        posting(),
        check_off(),
        mock.clone(),
        mock.clone(),
        Arc::new(JobProgress::new("j")),
        None,
        Some(sink.clone()),
    );
    let nzb = generator();
    p.post(
        &CancellationToken::new(),
        std::slice::from_ref(&file),
        &nzb,
        None,
    )
    .await
    .unwrap();
    assert_eq!(
        mock.posted_ids(),
        ["old2@x"],
        "only the missing article is re-posted, with its original id"
    );
    assert!(
        sink.recorded.lock().unwrap().is_empty(),
        "the manifest is not rewritten"
    );
    let xml = nzb.render().unwrap();
    assert_eq!(
        xml.matches("<segment ").count(),
        3,
        "present articles still appear in the NZB"
    );
    assert!(xml.contains("old0@x") && xml.contains("old1@x") && xml.contains("old2@x"));
    // the re-posted body is the right slice
    assert_eq!(
        decode_body(&mock.posts.lock().unwrap()[0]),
        std::fs::read(&file).unwrap()[2000..].to_vec()
    );
}

fn check_on(max_repost: u64, deferred_ms: i64) -> PostCheck {
    PostCheck {
        enabled: Some(true),
        max_repost,
        deferred_check_delay: crate::config::Duration(if deferred_ms > 0 {
            format!("{deferred_ms}ms")
        } else {
            String::new()
        }),
        ..Default::default()
    }
}

#[tokio::test]
async fn immediate_check_passes_when_everything_is_present() {
    let t = Tmp::new();
    let mock = Mock::new(4);
    let p = poster(&mock, check_on(1, 0), None, None);
    p.post(
        &CancellationToken::new(),
        &[t.file("a.bin", 2500)],
        &generator(),
        None,
    )
    .await
    .unwrap();
    assert_eq!(mock.stat_calls.load(Ordering::SeqCst), 3);
    assert_eq!(p.stats().articles_checked, 3);
}

#[tokio::test]
async fn failed_checks_are_reposted_with_the_same_message_ids() {
    let t = Tmp::new();
    let mock = Mock::new(4);
    // the first STAT round sees nothing; after a re-post the article becomes visible
    let p = poster(&mock, check_on(3, 0), None, None);
    mock.all_present.store(false, Ordering::SeqCst);
    let original_present = mock.present.lock().unwrap().len();
    assert_eq!(original_present, 0);
    // hide the first two articles from the first check round only
    struct Flaky(Arc<Mock>, AtomicUsize);
    impl ArticleClient for Flaky {
        fn post(&self, a: Vec<u8>) -> BoxFut<'_, Result<(), NntpError>> {
            self.0.post(a)
        }
        fn stat<'a>(&'a self, id: &'a str) -> BoxFut<'a, Result<bool, NntpError>> {
            Box::pin(async move {
                if self.1.fetch_add(1, Ordering::SeqCst) < 2 {
                    return Ok(false);
                }
                self.0.stat(id).await
            })
        }
        fn max_connections(&self) -> usize {
            4
        }
    }
    let flaky: Arc<dyn ArticleClient> = Arc::new(Flaky(mock.clone(), AtomicUsize::new(0)));
    let p2 = Poster::new(
        posting(),
        check_on(3, 0),
        mock.clone(),
        flaky,
        Arc::new(JobProgress::new("j")),
        None,
        None,
    );
    p2.post(
        &CancellationToken::new(),
        &[t.file("a.bin", 2500)],
        &generator(),
        None,
    )
    .await
    .unwrap();
    let mut ids = mock.posted_ids();
    ids.sort();
    ids.dedup();
    assert_eq!(
        ids.len(),
        3,
        "re-posts reuse the original ids (no new ids appear)"
    );
    assert_eq!(
        mock.posts.lock().unwrap().len(),
        5,
        "3 initial + 2 re-posts"
    );
    drop(p);
}

#[tokio::test]
async fn exhausted_reposts_defer_or_fail_depending_on_config() {
    let t = Tmp::new();
    let mock = Mock::new(4);
    let p = poster(&mock, check_on(2, 30_000), None, None);
    let nzb = generator();
    let file = t.file("a.bin", 1500);
    // find ids after posting: make every article permanently invisible
    mock.all_present.store(false, Ordering::SeqCst);
    struct Never;
    impl ArticleClient for Never {
        fn post(&self, _a: Vec<u8>) -> BoxFut<'_, Result<(), NntpError>> {
            Box::pin(async { Ok(()) })
        }
        fn stat<'a>(&'a self, _id: &'a str) -> BoxFut<'a, Result<bool, NntpError>> {
            Box::pin(async { Ok(false) })
        }
        fn max_connections(&self) -> usize {
            2
        }
    }
    let never: Arc<dyn ArticleClient> = Arc::new(Never);
    let deferring = Poster::new(
        posting(),
        check_on(2, 30_000),
        mock.clone(),
        never.clone(),
        Arc::new(JobProgress::new("j")),
        None,
        None,
    );
    let e = deferring
        .post(
            &CancellationToken::new(),
            std::slice::from_ref(&file),
            &nzb,
            None,
        )
        .await
        .unwrap_err();
    match e {
        PostError::Deferred(d) => {
            assert_eq!(
                (d.failed_articles.len(), d.total_articles),
                (2, 4),
                "2 articles deferred; 2 checked in each of 2 passes counted"
            );
            assert!(d.failed_articles.iter().all(|f| f.groups == ["alt.test"]));
            assert_eq!(
                e_to_string(&PostError::Deferred(d)),
                "2/4 articles deferred for later verification"
            );
        }
        other => panic!("expected a deferred error, got {other:?}"),
    }
    let failing = Poster::new(
        posting(),
        check_on(2, 0),
        mock.clone(),
        never,
        Arc::new(JobProgress::new("j")),
        None,
        None,
    );
    let e = failing
        .post(&CancellationToken::new(), &[file], &generator(), None)
        .await
        .unwrap_err();
    assert!(matches!(&e, PostError::Verify { retries: 2, .. }), "{e:?}");
    assert!(
        e.to_string().contains("failed to verify file")
            && e.to_string().ends_with("after 2 retries")
    );
    drop(p);
}

fn e_to_string(e: &PostError) -> String {
    e.to_string()
}

#[tokio::test]
async fn the_reposter_reuses_the_message_id_and_body() {
    let t = Tmp::new();
    let file = t.file("src.bin", 2500);
    let mock = Mock::new(2);
    let engine = Arc::new(Engine::new(1000, 0, 2));
    let client: Arc<dyn ArticleClient> = mock.clone();
    let r = Reposter::new(Some(client), engine.clone(), 0);
    let rec = ArticleRecord {
        index: 1,
        source_path: file.to_string_lossy().into_owned(),
        file_role: "original".into(),
        offset: 1000,
        body_size: 1000,
        message_id: "keep@me".into(),
        subject: "subj".into(),
        original_subject: "osubj".into(),
        from: "f <f@x.y>".into(),
        groups: vec!["alt.test".into()],
        date: chrono::DateTime::parse_from_rfc3339("2023-11-14T22:13:20Z").unwrap(),
        custom_headers: BTreeMap::new(),
        xnxg_header: String::new(),
        file_name: "src.bin".into(),
        part_number: 2,
        total_parts: 3,
        file_size: 2500,
    };
    r.repost(&CancellationToken::new(), &rec).await.unwrap();
    let wire = mock.posts.lock().unwrap()[0].clone();
    assert_eq!(header(&wire, "Message-ID"), "<keep@me>");
    assert_eq!(
        decode_body(&wire),
        std::fs::read(&file).unwrap()[1000..2000].to_vec()
    );
    assert_eq!(
        (r.stats().articles_posted, engine.metrics().reserved_bytes),
        (1, 0)
    );

    let mut missing = rec.clone();
    missing.source_path = t.0.join("gone.bin").to_string_lossy().into_owned();
    assert!(
        r.repost(&CancellationToken::new(), &missing)
            .await
            .unwrap_err()
            .to_string()
            .starts_with("repost: open source")
    );
    let none = Reposter::new(None, engine, 0);
    assert_eq!(
        none.repost(&CancellationToken::new(), &rec)
            .await
            .unwrap_err()
            .to_string(),
        "reposter has no upload pool"
    );
}

#[tokio::test]
async fn short_reads_fail_cleanly() {
    let t = Tmp::new();
    let f = Arc::new(File::open(t.file("a", 100)).unwrap());
    assert_eq!(
        read_at_with_stall_guard(&f, 10, 20, &CancellationToken::new())
            .await
            .unwrap()
            .len(),
        20
    );
    assert!(
        read_at_with_stall_guard(&f, 90, 50, &CancellationToken::new())
            .await
            .is_err()
    );
}
