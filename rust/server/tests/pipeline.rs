mod support;
use support::*;

#[tokio::test]
async fn an_upload_over_the_old_2_mib_limit_is_posted_verified_and_announced_over_the_websocket() {
    let fake = fake_nntp().await;
    let dir = temp_dir("happy");
    let web = Web::start(&dir, &fake).await;
    let ev = web.events().await;
    let r = web.upload(vec![("big.bin".into(), blob(3_000_000))]).await;
    assert!(r.status().is_success(), "{}", r.status());
    let item = web.wait_status("big.bin", "complete", 30).await;
    assert!(
        fake.posts() >= 30,
        "3 MB / 100 kB articles, got {}",
        fake.posts()
    );
    ev.wait_for("upload-complete", 5).await;
    assert!(ev.count("queue-updated") >= 1, "{:?}", ev.kinds());
    let nzb = item["nzbPath"].as_str().expect("nzb path");
    assert!(std::path::Path::new(nzb).exists(), "{nzb}");
    wait("verified", 30, || async {
        let s = web.get("/api/queue/stats").await;
        (s["pendingVerification"] == 0 && s["verificationFailed"] == 0).then_some(())
    })
    .await;
    web.stop().await;
}

#[tokio::test]
async fn a_slow_job_shows_up_in_running_jobs_pushes_and_then_clears() {
    let fake = fake_nntp().await;
    // longer than the 1 s broadcaster tick, so the running job is always observed
    fake.set_post(PostMode::Delay(1500));
    let dir = temp_dir("slow");
    let web = Web::start(&dir, &fake).await;
    let ev = web.events().await;
    web.upload(vec![("slow.bin".into(), blob(1_000_000))]).await;
    web.wait_status("slow.bin", "complete", 60).await;
    wait("running jobs cleared", 10, || async {
        let all = ev.all();
        let last = all.iter().rev().find(|e| e.0 == "running-jobs-updated")?;
        last.1.as_array().is_some_and(Vec::is_empty).then_some(())
    })
    .await;
    let with_job = ev
        .all()
        .iter()
        .any(|e| e.0 == "running-jobs-updated" && e.1.as_array().is_some_and(|a| !a.is_empty()));
    assert!(
        with_job,
        "a non-empty running-jobs push was seen: {:?}",
        ev.kinds()
    );
    assert!(ev.count("nntp-pool-metrics-updated") <= 4);
    web.stop().await;
}

async fn status_of(web: &Web, name: &str) -> String {
    web.queue_items()
        .await
        .into_iter()
        .find(|i| i["fileName"] == name)
        .map(|i| i["status"].as_str().unwrap_or("").to_string())
        .unwrap_or_default()
}

/// Waits for `name` to leave pending/running, so a stuck job fails the test instead of hanging it.
async fn settle(web: &Web, name: &str, secs: u64) -> String {
    wait(
        &format!("{name} reaches a terminal state"),
        secs,
        || async {
            let s = status_of(web, name).await;
            (s == "complete" || s == "error").then_some(s)
        },
    )
    .await
}

#[tokio::test]
async fn a_server_that_rejects_every_post_ends_in_error_with_the_reason_and_nothing_stays_running()
{
    let fake = fake_nntp().await;
    fake.set_post(PostMode::Reject(441));
    let dir = temp_dir("reject");
    let web = Web::start(&dir, &fake).await;
    let ev = web.events().await;
    web.upload(vec![("rej.bin".into(), blob(200_000))]).await;
    assert_eq!(settle(&web, "rej.bin", 60).await, "error");
    let item = web.queue_items().await.remove(0);
    assert!(
        item["errorMessage"].as_str().is_some_and(|m| !m.is_empty()),
        "{item}"
    );
    assert_eq!(web.get("/api/processor/status").await["runningJobs"], 0);
    assert!(ev.count("job-error") >= 1, "{:?}", ev.kinds());
    web.stop().await;
}

#[tokio::test]
async fn a_connection_dropped_after_every_article_body_ends_in_error_not_a_hang() {
    let fake = fake_nntp().await;
    fake.set_post(PostMode::DropAfterBody);
    let dir = temp_dir("drop");
    let web = Web::start(&dir, &fake).await;
    web.upload(vec![("drop.bin".into(), blob(200_000))]).await;
    assert_eq!(settle(&web, "drop.bin", 60).await, "error");
    assert_eq!(web.get("/api/processor/status").await["runningJobs"], 0);
    web.stop().await;
}

/// Takes minutes: the poster waits out its 120 s per-article timeout, twice. Run with `--ignored`.
#[tokio::test]
#[ignore = "slow: waits out the 120 s POST timeout"]
async fn a_server_that_never_answers_a_post_is_given_up_on_in_bounded_time() {
    let fake = fake_nntp().await;
    fake.set_post(PostMode::Hang);
    let dir = temp_dir("hang");
    let web = Web::start(&dir, &fake).await;
    web.upload(vec![("hang.bin".into(), blob(150_000))]).await;
    assert_eq!(settle(&web, "hang.bin", 600).await, "error");
    web.stop().await;
}

#[tokio::test]
async fn articles_that_never_show_up_end_verification_failed_not_pending_forever() {
    let fake = fake_nntp().await;
    fake.set_stat(StatMode::Missing);
    let dir = temp_dir("missing");
    let web = Web::start(&dir, &fake).await;
    web.upload(vec![("gone.bin".into(), blob(300_000))]).await;
    web.wait_status("gone.bin", "complete", 30).await;
    wait("verification gives up", 90, || async {
        let s = web.get("/api/queue/stats").await;
        (s["pendingVerification"] == 0 && s["verificationFailed"].as_i64() >= Some(1)).then_some(())
    })
    .await;
    assert!(
        fake.posts() > 3,
        "missing articles are reposted: {}",
        fake.posts()
    );
    web.stop().await;
}

#[tokio::test]
async fn pausing_holds_new_uploads_and_resuming_runs_them() {
    let fake = fake_nntp().await;
    let dir = temp_dir("pause");
    let web = Web::start(&dir, &fake).await;
    assert!(
        web.post_json("/api/processor/pause", &serde_json::json!({}))
            .await
            .status()
            .is_success()
    );
    assert_eq!(web.get("/api/processor/paused").await["paused"], true);
    web.upload(vec![("held.bin".into(), blob(100_000))]).await;
    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    assert_eq!(status_of(&web, "held.bin").await, "pending");
    assert_eq!(fake.posts(), 0);
    assert!(
        web.post_json("/api/processor/resume", &serde_json::json!({}))
            .await
            .status()
            .is_success()
    );
    web.wait_status("held.bin", "complete", 30).await;
    web.stop().await;
}

#[tokio::test]
async fn cancelling_a_running_job_stops_it_and_frees_the_slot() {
    let fake = fake_nntp().await;
    fake.set_post(PostMode::Delay(400));
    let dir = temp_dir("cancel");
    let web = Web::start(&dir, &fake).await;
    web.upload(vec![("c.bin".into(), blob(2_000_000))]).await;
    let id = wait("job running", 20, || async {
        web.get("/api/running-jobs").await.as_array()?.first()?["id"]
            .as_str()
            .map(String::from)
    })
    .await;
    let r = web
        .http
        .delete(format!("{}/api/queue/{id}/cancel", web.base))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "{}", r.status());
    wait("slot freed", 30, || async {
        (web.get("/api/processor/status").await["runningJobs"] == 0).then_some(())
    })
    .await;
    web.stop().await;
}

#[tokio::test]
async fn a_job_interrupted_by_shutdown_is_picked_up_again_after_a_restart() {
    let fake = fake_nntp().await;
    fake.set_post(PostMode::Hang);
    let dir = temp_dir("restart");
    let web = Web::start(&dir, &fake).await;
    web.upload(vec![("r.bin".into(), blob(300_000))]).await;
    wait("job running", 20, || async {
        (web.get("/api/processor/status").await["runningJobs"] == 1).then_some(())
    })
    .await;
    web.stop().await;
    fake.set_post(PostMode::Accept);
    let web = Web::restart(&dir).await;
    assert_eq!(
        settle(&web, "r.bin", 60).await,
        "complete",
        "the interrupted job is not lost"
    );
    web.stop().await;
}

#[tokio::test]
async fn many_files_unicode_names_and_an_empty_file_all_reach_a_terminal_state() {
    let fake = fake_nntp().await;
    let dir = temp_dir("many");
    let web = Web::start(&dir, &fake).await;
    let mut files: Vec<(String, Vec<u8>)> = (0..6)
        .map(|i| (format!("part-{i}.bin"), blob(120_000 + i * 1000)))
        .collect();
    files.push(("ñandú — 日本語 [1].bin".into(), blob(90_000)));
    files.push(("empty.bin".into(), Vec::new()));
    let names: Vec<String> = files.iter().map(|f| f.0.clone()).collect();
    assert!(web.upload(files).await.status().is_success());
    for n in &names {
        let s = settle(&web, n, 60).await;
        if n != "empty.bin" {
            assert_eq!(s, "complete", "{n}");
        }
    }
    assert_eq!(web.get("/api/processor/status").await["runningJobs"], 0);
    web.stop().await;
}
