//! Stuck connections: servers that go silent at every stage of the NNTP exchange. Each case must end
//! in a bounded time, either recovered (the upload completes and downloads back whole) or as an
//! error with a reason, never as a job that runs forever. Timeouts are shortened process-wide so
//! each case takes seconds.

mod support;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use postie_core::timeouts::{self, Timeouts};
use serde_json::Value;
use support::download;
use support::*;

const A: usize = 100_000;

fn short_timeouts() {
    timeouts::set_for_tests(Timeouts {
        // production proportions (120 s / 120 s / 30 s): the whole connect budget, three
        // attempts, fits inside one post timeout
        post: Duration::from_secs(4),
        stall: Duration::from_secs(4),
        connect: Duration::from_secs(1),
    });
}

async fn start(tag: &str, fake: &Fake) -> Web {
    short_timeouts();
    let dir = temp_dir(tag);
    Web::start(&dir, fake).await
}

fn add_file(web: &Web, name: &str, len: usize) -> (PathBuf, Vec<u8>) {
    let p = web.dir.join("src").join(name);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    let data = blob(len);
    std::fs::write(&p, &data).unwrap();
    web.app
        .handle_dropped_files(&[p.to_string_lossy().into_owned()])
        .expect("add file");
    (p, data)
}

async fn item(web: &Web, name: &str) -> Option<Value> {
    web.queue_items()
        .await
        .into_iter()
        .find(|i| i["fileName"].as_str().is_some_and(|n| n.contains(name)))
}

/// Waits (bounded) for `name` to reach complete or error; returns the item.
async fn settle(web: &Web, name: &str, secs: u64) -> Value {
    wait(&format!("{name} settles"), secs, || async {
        let i = item(web, name).await?;
        (i["status"] == "complete" || i["status"] == "error").then_some(i)
    })
    .await
}

async fn assert_error_with_reason(web: &Web, name: &str, secs: u64) {
    let i = settle(web, name, secs).await;
    assert_eq!(i["status"], "error", "{i}");
    assert!(
        i["errorMessage"].as_str().is_some_and(|m| !m.is_empty()),
        "the error carries a reason: {i}"
    );
    assert_eq!(
        web.get("/api/processor/status").await["runningJobs"],
        0,
        "nothing stays running"
    );
}

async fn assert_downloads_whole(web: &Web, fake: &Fake, name: &str, data: &[u8]) {
    let i = settle(web, name, 90).await;
    assert_eq!(i["status"], "complete", "{i}");
    wait("verification settled", 90, || async {
        let s = web.get("/api/queue/stats").await;
        assert_eq!(s["verificationFailed"], 0, "{s}");
        (s["pendingVerification"] == 0).then_some(())
    })
    .await;
    let xml = download::read_nzb(Path::new(i["nzbPath"].as_str().unwrap()));
    download::assert_same_files(
        &download::download(&xml, fake),
        &[(name.to_string(), data.to_vec())],
    );
}

#[tokio::test]
async fn a_server_that_accepts_but_never_greets_fails_the_upload_in_bounded_time() {
    let fake = fake_nntp().await;
    let web = start("st-greet", &fake).await;
    fake.set_conn(ConnMode::NoGreeting);
    add_file(&web, "greet.bin", 2 * A);
    assert_error_with_reason(&web, "greet.bin", 120).await;
    web.stop().await;
}

#[tokio::test]
async fn a_server_that_never_answers_authentication_fails_the_upload_in_bounded_time() {
    let fake = fake_nntp().await;
    short_timeouts();
    let dir = temp_dir("st-auth");
    let mut cfg = fast_config(&dir, fake.addr.port());
    let s = &mut cfg.servers.as_mut().unwrap()[0];
    s.username = "u".into();
    s.password = "p".into();
    let web = Web::start_with(&dir, cfg).await;
    fake.set_conn(ConnMode::HangAuth);
    add_file(&web, "auth.bin", 2 * A);
    assert_error_with_reason(&web, "auth.bin", 120).await;
    web.stop().await;
}

#[tokio::test]
async fn a_server_that_stops_reading_mid_article_fails_the_upload_in_bounded_time() {
    let fake = fake_nntp().await;
    short_timeouts();
    let dir = temp_dir("st-backpressure");
    let mut cfg = fast_config(&dir, fake.addr.port());
    // articles far larger than the socket buffers, so the client's writes really block
    cfg.posting.article_size_in_bytes = 8_000_000;
    let web = Web::start_with(&dir, cfg).await;
    fake.set_conn(ConnMode::StopReadingBody);
    add_file(&web, "full.bin", 16_000_000);
    assert_error_with_reason(&web, "full.bin", 90).await;
    web.stop().await;
}

#[tokio::test]
async fn a_server_that_never_answers_any_post_fails_the_upload_in_bounded_time() {
    let fake = fake_nntp().await;
    let web = start("st-hang-all", &fake).await;
    fake.set_post(PostMode::Hang);
    add_file(&web, "hang.bin", 2 * A);
    assert_error_with_reason(&web, "hang.bin", 120).await;
    web.stop().await;
}

#[tokio::test]
async fn one_post_that_never_gets_an_answer_is_retried_and_the_upload_completes() {
    let fake = fake_nntp().await;
    let web = start("st-hang-one", &fake).await;
    fake.hang_posts(&[2]);
    let (_, data) = add_file(&web, "one.bin", 6 * A);
    assert_downloads_whole(&web, &fake, "one.bin", &data).await;
    web.stop().await;
}

#[tokio::test]
async fn a_server_that_is_down_fails_the_upload_with_a_reason_in_bounded_time() {
    let fake = fake_nntp().await;
    let web = start("st-down", &fake).await;
    fake.go_down();
    // let the listener close before the upload starts
    tokio::time::sleep(Duration::from_millis(200)).await;
    add_file(&web, "down.bin", 2 * A);
    assert_error_with_reason(&web, "down.bin", 120).await;
    web.stop().await;
}

#[tokio::test]
async fn verification_waits_out_a_server_that_hangs_on_stat_and_finishes_once_it_answers() {
    let fake = fake_nntp().await;
    let web = start("st-stat", &fake).await;
    fake.set_stat(StatMode::Hang);
    let (_, data) = add_file(&web, "stat.bin", 3 * A);
    let i = settle(&web, "stat.bin", 60).await;
    assert_eq!(i["status"], "complete", "{i}");
    // several check rounds against a silent server
    tokio::time::sleep(Duration::from_secs(6)).await;
    let s = web.get("/api/queue/stats").await;
    assert_eq!(
        s["verificationFailed"], 0,
        "a silent server is not proof of loss: {s}"
    );
    let posts = fake.posts();
    assert!(
        posts < 3 * 4,
        "a silent STAT does not trigger reposts: {posts}"
    );
    fake.set_stat(StatMode::Stored);
    assert_downloads_whole(&web, &fake, "stat.bin", &data).await;
    web.stop().await;
}

#[tokio::test]
async fn cancelling_a_job_stuck_on_a_silent_server_frees_its_slot_quickly() {
    let fake = fake_nntp().await;
    let web = start("st-cancel", &fake).await;
    fake.set_post(PostMode::Hang);
    add_file(&web, "c.bin", 3 * A);
    let id = wait("job running", 20, || async {
        web.get("/api/running-jobs").await.as_array()?.first()?["id"]
            .as_str()
            .map(String::from)
    })
    .await;
    let t = Instant::now();
    let r = web
        .http
        .delete(format!("{}/api/queue/{id}/cancel", web.base))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "{}", r.status());
    wait("slot freed", 10, || async {
        (web.get("/api/processor/status").await["runningJobs"] == 0).then_some(())
    })
    .await;
    assert!(t.elapsed() < Duration::from_secs(10));
    web.stop().await;
}

#[tokio::test]
async fn shutting_down_while_every_connection_is_stuck_finishes_promptly() {
    let fake = fake_nntp().await;
    let web = start("st-shutdown", &fake).await;
    fake.set_post(PostMode::Hang);
    add_file(&web, "s.bin", 3 * A);
    wait("job running", 20, || async {
        (web.get("/api/processor/status").await["runningJobs"] == 1).then_some(())
    })
    .await;
    tokio::time::timeout(Duration::from_secs(20), web.stop())
        .await
        .expect("shutdown hung on a stuck connection");
}

#[tokio::test]
async fn a_server_that_goes_silent_then_recovers_lets_the_next_upload_through() {
    let fake = fake_nntp().await;
    let web = start("st-recover", &fake).await;
    fake.set_post(PostMode::Hang);
    add_file(&web, "first.bin", 2 * A);
    assert_error_with_reason(&web, "first.bin", 120).await;
    fake.set_post(PostMode::Accept);
    let (_, data) = add_file(&web, "second.bin", 4 * A);
    assert_downloads_whole(&web, &fake, "second.bin", &data).await;
    web.stop().await;
}
