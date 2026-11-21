//! Posting features end to end: several providers (one failing), a verify-only server, the upload
//! throttle, posting headers and the folder watcher. Every upload is downloaded back whole.

mod support;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use postie_core::config::{self, ConfigData, CustomHeader, NewsgroupConfig, ServerConfig};
use postie_core::timeouts::{self, Timeouts};
use serde_json::Value;
use support::download;
use support::*;

const A: usize = 100_000;

fn server(fake: &Fake, role: &str) -> ServerConfig {
    ServerConfig {
        host: "127.0.0.1".into(),
        port: i64::from(fake.addr.port()),
        role: role.into(),
        // providers must be distinct accounts; the fake accepts any login
        username: format!("user{}", fake.addr.port()),
        password: "p".into(),
        max_connections: 2,
        ..Default::default()
    }
}

fn write(dir: &Path, name: &str, len: usize) -> (PathBuf, Vec<u8>) {
    let p = dir.join(name);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let data = blob(len);
    std::fs::write(&p, &data).unwrap();
    (p, data)
}

fn add(web: &Web, p: &Path) {
    web.app
        .handle_dropped_files(&[p.to_string_lossy().into_owned()])
        .expect("add file");
}

/// Waits for `name` to complete and verify; returns the item.
async fn complete(web: &Web, name: &str, secs: u64) -> Value {
    let i = wait(&format!("{name} complete"), secs, || async {
        let i = web
            .queue_items()
            .await
            .into_iter()
            .find(|i| i["fileName"].as_str().is_some_and(|n| n.contains(name)))?;
        assert_ne!(i["status"], "error", "{i}");
        (i["status"] == "complete").then_some(i)
    })
    .await;
    wait("verification settled", secs, || async {
        let s = web.get("/api/queue/stats").await;
        assert_eq!(s["verificationFailed"], 0, "{s}");
        (s["pendingVerification"] == 0).then_some(())
    })
    .await;
    i
}

fn assert_whole(item: &Value, fake: &Fake, name: &str, data: &[u8]) {
    let xml = download::read_nzb(Path::new(item["nzbPath"].as_str().unwrap()));
    download::assert_same_files(
        &download::download(&xml, fake),
        &[(name.to_string(), data.to_vec())],
    );
}

/// Merges the article stores of several fakes into the first, so one download can read them all.
fn merge_into(target: &Fake, others: &[&Fake]) {
    for o in others {
        for id in o.article_ids() {
            let a = o.article(&id).unwrap();
            target.ctl.articles.lock().unwrap().insert(id, a);
        }
    }
}

#[tokio::test]
async fn uploads_are_spread_over_two_providers_and_download_back_whole() {
    let (a, b) = (fake_nntp().await, fake_nntp().await);
    a.propagate_to(&b);
    b.propagate_to(&a);
    let dir = temp_dir("ft-two");
    let mut cfg = fast_config(&dir, a.addr.port());
    cfg.servers = Some(vec![server(&a, "upload"), server(&b, "upload")]);
    let web = Web::start_with(&dir, cfg).await;
    let (p, data) = write(&dir.join("src"), "two.bin", 12 * A);
    add(&web, &p);
    let item = complete(&web, "two.bin", 60).await;
    assert!(
        a.posts() > 0 && b.posts() > 0,
        "both providers carry posts: {} / {}",
        a.posts(),
        b.posts()
    );
    assert_whole(&item, &a, "two.bin", &data);
    web.stop().await;
}

/// One of two providers fails the way `how` sets up; the upload must finish on the other.
async fn failover(tag: &str, how: impl FnOnce(&Fake)) {
    timeouts::set_for_tests(Timeouts {
        // production proportions (120 s / 120 s / 30 s): the whole connect budget, three
        // attempts, fits inside one post timeout
        post: Duration::from_secs(4),
        stall: Duration::from_secs(4),
        connect: Duration::from_secs(1),
    });
    let (a, b) = (fake_nntp().await, fake_nntp().await);
    let dir = temp_dir(tag);
    let mut cfg = fast_config(&dir, a.addr.port());
    cfg.servers = Some(vec![server(&a, "upload"), server(&b, "upload")]);
    let web = Web::start_with(&dir, cfg).await;
    how(&b);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (p, data) = write(&dir.join("src"), "failover.bin", 10 * A);
    add(&web, &p);
    let item = complete(&web, "failover.bin", 120).await;
    merge_into(&a, &[&b]);
    assert_whole(&item, &a, "failover.bin", &data);
    web.stop().await;
}

#[tokio::test]
async fn when_one_of_two_providers_goes_down_uploads_finish_on_the_other() {
    failover("ft-failover", Fake::go_down).await;
}

/// A black-holed host: connections open but nothing ever answers, slower than any post timeout.
#[tokio::test]
async fn when_one_of_two_providers_never_answers_uploads_finish_on_the_other() {
    failover("ft-blackhole", |f| f.set_conn(ConnMode::NoGreeting)).await;
}

#[tokio::test]
async fn a_verify_only_server_receives_the_checks_and_never_a_post() {
    let (up, check) = (fake_nntp().await, fake_nntp().await);
    up.propagate_to(&check);
    let dir = temp_dir("ft-verify");
    let mut cfg = fast_config(&dir, up.addr.port());
    cfg.servers = Some(vec![server(&up, "upload"), server(&check, "verify")]);
    let web = Web::start_with(&dir, cfg).await;
    let (p, data) = write(&dir.join("src"), "v.bin", 4 * A);
    add(&web, &p);
    let item = complete(&web, "v.bin", 60).await;
    assert_eq!(check.posts(), 0, "the verify server is never posted to");
    assert!(check.stats() > 0, "the verify server answers the checks");
    assert_eq!(
        up.stats(),
        0,
        "checks go to the verify server, not the upload one"
    );
    assert_whole(&item, &up, "v.bin", &data);
    web.stop().await;
}

#[tokio::test]
async fn the_upload_throttle_caps_the_rate() {
    let fake = fake_nntp().await;
    let dir = temp_dir("ft-throttle");
    let mut cfg = fast_config(&dir, fake.addr.port());
    let rate = 400_000;
    cfg.posting.throttle_rate = rate;
    let web = Web::start_with(&dir, cfg).await;
    let len = 12 * A; // 3 s at the cap
    let (p, data) = write(&dir.join("src"), "slow.bin", len);
    let t = Instant::now();
    add(&web, &p);
    let item = complete(&web, "slow.bin", 60).await;
    let took = t.elapsed();
    // a one-second burst allowance at most: never faster than (len - rate) / rate
    let floor = Duration::from_secs_f64((len as f64 - rate as f64) / rate as f64);
    assert!(
        took >= floor,
        "{len} bytes at {rate} B/s took only {took:?}"
    );
    assert_whole(&item, &fake, "slow.bin", &data);
    web.stop().await;
}

fn header_block(raw: &[u8]) -> String {
    let sep = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    String::from_utf8_lossy(&raw[..sep + 2]).into_owned()
}

#[tokio::test]
async fn posting_headers_groups_and_the_nxg_header_reach_every_article() {
    let fake = fake_nntp().await;
    let dir = temp_dir("ft-headers");
    let mut cfg: ConfigData = fast_config(&dir, fake.addr.port());
    cfg.posting.obfuscation_policy = "none".into();
    cfg.posting.groups = Some(vec![
        NewsgroupConfig {
            name: "alt.binaries.one".into(),
            enabled: Some(true),
        },
        NewsgroupConfig {
            name: "alt.binaries.two".into(),
            enabled: Some(true),
        },
    ]);
    cfg.posting.group_policy = "all".into();
    cfg.posting.post_headers.default_from = "Tester <tester@example.invalid>".into();
    cfg.posting.post_headers.add_nxg_header = true;
    cfg.posting.post_headers.custom_headers = Some(vec![CustomHeader {
        name: "X-Postie-Test".into(),
        value: "round-trip".into(),
    }]);
    let web = Web::start_with(&dir, cfg).await;
    let (p, data) = write(&dir.join("src"), "headers.bin", 3 * A);
    add(&web, &p);
    let item = complete(&web, "headers.bin", 60).await;
    let ids = fake.article_ids();
    assert!(!ids.is_empty());
    for id in ids {
        let h = header_block(&fake.article(&id).unwrap());
        assert!(
            h.contains("From: Tester <tester@example.invalid>\r\n"),
            "{h}"
        );
        assert!(
            h.contains("Newsgroups: alt.binaries.one,alt.binaries.two\r\n"),
            "{h}"
        );
        assert!(h.contains("X-Postie-Test: round-trip\r\n"), "{h}");
        assert!(h.contains("X-Nxg: "), "{h}");
        assert!(h.contains(&format!("Message-ID: <{id}>\r\n")), "{h}");
        assert!(h.contains("Subject: ") && h.contains("headers.bin"), "{h}");
    }
    assert_whole(&item, &fake, "headers.bin", &data);
    web.stop().await;
}

#[tokio::test]
async fn the_watcher_uploads_new_files_skips_ignored_and_small_ones_and_deletes_originals() {
    let fake = fake_nntp().await;
    let dir = temp_dir("ft-watch");
    let watch = dir.join("watch");
    let mut cfg = fast_config(&dir, fake.addr.port());
    let mut w = config::default_config().watcher.clone();
    w.enabled = true;
    w.name = "test".into();
    w.watch_directory = watch.to_string_lossy().into_owned();
    w.check_interval = config::Duration::new("1s");
    w.min_file_age = config::Duration::new("0s"); // no wait
    w.min_file_size = 1000;
    w.ignore_patterns = Some(vec!["*.tmp".into()]);
    w.delete_original_file = true;
    cfg.watchers = Some(vec![w]);
    let web = Web::start_with(&dir, cfg).await;
    let (keep, data) = write(&watch, "show.bin", 3 * A);
    let (ignored, _) = write(&watch, "partial.tmp", 3 * A);
    let (small, _) = write(&watch, "tiny.bin", 10);
    let item = complete(&web, "show.bin", 90).await;
    assert_whole(&item, &fake, "show.bin", &data);
    wait("original deleted after verification", 30, || async {
        (!keep.exists()).then_some(())
    })
    .await;
    // give the watcher a few more scans to (wrongly) pick the others up
    tokio::time::sleep(Duration::from_secs(3)).await;
    let names: Vec<String> = web
        .queue_items()
        .await
        .iter()
        .filter_map(|i| i["fileName"].as_str().map(String::from))
        .collect();
    assert!(
        !names.iter().any(|n| n.contains("partial.tmp")),
        "ignored file queued: {names:?}"
    );
    assert!(
        !names.iter().any(|n| n.contains("tiny.bin")),
        "below-minimum file queued: {names:?}"
    );
    assert!(
        ignored.exists() && small.exists(),
        "skipped files are left alone"
    );
    web.stop().await;
}
