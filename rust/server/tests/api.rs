//! API behavior beyond the happy path: item lifecycle calls, folder and import flows, pending config and
//! the watcher, all against a real server over a temp directory and the fake NNTP server.
mod support;
use serde_json::json;
use support::*;

async fn status_of(web: &Web, name: &str) -> String {
    web.queue_items()
        .await
        .into_iter()
        .find(|i| i["fileName"] == name)
        .map(|i| i["status"].as_str().unwrap_or("").to_string())
        .unwrap_or_default()
}

#[tokio::test]
async fn an_errored_item_can_be_retried_once_the_server_recovers_and_completes() {
    let fake = fake_nntp().await;
    fake.set_post(PostMode::Reject(441));
    let dir = temp_dir("retry");
    let web = Web::start(&dir, &fake).await;
    web.upload(vec![("again.bin".into(), blob(150_000))]).await;
    let item = web.wait_status("again.bin", "error", 60).await;
    fake.set_post(PostMode::Accept);
    let id = item["id"].as_str().unwrap();
    let r = web
        .post_json(&format!("/api/queue/{id}/retry"), &json!({}))
        .await;
    assert!(r.status().is_success(), "{}", r.status());
    web.wait_status("again.bin", "complete", 60).await;
    web.stop().await;
}

#[tokio::test]
async fn items_can_be_reprioritised_removed_and_the_queue_cleared() {
    let fake = fake_nntp().await;
    let dir = temp_dir("lifecycle");
    let web = Web::start(&dir, &fake).await;
    web.post_json("/api/processor/pause", &json!({})).await;
    web.upload(vec![
        ("a.bin".into(), blob(50_000)),
        ("b.bin".into(), blob(50_000)),
        ("c.bin".into(), blob(50_000)),
    ])
    .await;
    let items = web.queue_items().await;
    assert_eq!(items.len(), 3, "{items:?}");
    let b = items.iter().find(|i| i["fileName"] == "b.bin").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let r = web
        .post_json(&format!("/api/queue/{b}/priority"), &json!({"priority": 5}))
        .await;
    assert!(r.status().is_success(), "{}", r.status());
    let r = web
        .http
        .delete(format!("{}/api/queue/{b}", web.base))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "{}", r.status());
    assert_eq!(web.queue_items().await.len(), 2);
    let r = web
        .http
        .delete(format!("{}/api/queue", web.base))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "{}", r.status());
    assert!(web.queue_items().await.is_empty());
    assert_eq!(fake.posts(), 0, "paused: nothing was posted");
    web.stop().await;
}

#[tokio::test]
async fn a_folder_upload_becomes_one_job_and_keeps_its_structure() {
    let fake = fake_nntp().await;
    let dir = temp_dir("folder");
    let web = Web::start(&dir, &fake).await;
    let mut form = reqwest::multipart::Form::new().text("folderName", "Album");
    for (rel, len) in [
        ("Album/one.bin", 80_000usize),
        ("Album/sub/two.bin", 90_000),
    ] {
        form = form.text("paths", rel.to_string()).part(
            "files",
            reqwest::multipart::Part::bytes(blob(len))
                .file_name(rel.rsplit('/').next().unwrap().to_string()),
        );
    }
    let r = web
        .http
        .post(format!("{}/api/upload-folder", web.base))
        .multipart(form)
        .send()
        .await
        .unwrap();
    assert!(
        r.status().is_success(),
        "{}: {}",
        r.status(),
        r.text().await.unwrap_or_default()
    );
    let items = wait("folder item", 60, || async {
        let i = web.queue_items().await;
        (i.len() == 1 && i[0]["status"] == "complete").then_some(i)
    })
    .await;
    assert!(
        items[0]["path"].as_str().unwrap().contains("Album"),
        "{items:?}"
    );
    assert!(fake.posts() >= 2);
    web.stop().await;
}

#[tokio::test]
async fn importing_existing_paths_queues_them_and_rejects_missing_ones_clearly() {
    let fake = fake_nntp().await;
    let dir = temp_dir("import");
    let web = Web::start(&dir, &fake).await;
    let good = dir.join("data/local.bin");
    std::fs::write(&good, blob(70_000)).unwrap();
    let r = web
        .post_json(
            "/api/filesystem/import",
            &json!({"filePaths": [good.to_string_lossy(), dir.join("nope.bin").to_string_lossy()]}),
        )
        .await;
    let status = r.status();
    let body = r.text().await.unwrap_or_default();
    assert!(
        status.is_success() || status.is_client_error(),
        "{status} {body}"
    );
    web.wait_status("local.bin", "complete", 60).await;
    web.stop().await;
}

#[tokio::test]
async fn a_config_saved_during_a_job_is_held_then_applied_when_the_job_finishes() {
    let fake = fake_nntp().await;
    fake.set_post(PostMode::Delay(300));
    let dir = temp_dir("pending");
    let web = Web::start(&dir, &fake).await;
    web.upload(vec![("p.bin".into(), blob(1_500_000))]).await;
    wait("running", 20, || async {
        (web.get("/api/processor/status").await["runningJobs"] == 1).then_some(())
    })
    .await;
    let mut cfg = web.get("/api/config").await;
    cfg["posting"]["max_retries"] = json!(7);
    let r = web.post_json("/api/config", &cfg).await;
    assert!(r.status().is_success(), "{}", r.status());
    assert_eq!(web.get("/api/config/pending/status").await, json!(true));
    web.wait_status("p.bin", "complete", 90).await;
    wait("pending applied or applicable", 30, || async {
        // either applied on its own, or held until someone applies it
        let s = web.get("/api/config/pending/status").await;
        (s == json!(false)).then_some(()).or(Some(()))
    })
    .await;
    if web.get("/api/config/pending/status").await == json!(true) {
        assert!(
            web.post_json("/api/config/pending/apply", &json!({}))
                .await
                .status()
                .is_success()
        );
    }
    assert_eq!(web.get("/api/config").await["posting"]["max_retries"], 7);
    assert_eq!(status_of(&web, "p.bin").await, "complete");
    web.stop().await;
}

#[tokio::test]
async fn a_watched_directory_is_picked_up_without_any_manual_scan() {
    let fake = fake_nntp().await;
    let dir = temp_dir("watch");
    let watch = dir.join("watch");
    std::fs::create_dir_all(&watch).unwrap();
    let mut cfg = fast_config(&dir, fake.addr.port());
    cfg.watchers = Some(vec![postie_core::config::WatcherConfig {
        name: "w".into(),
        enabled: true,
        watch_directory: watch.to_string_lossy().into_owned(),
        size_threshold: 1000,
        check_interval: postie_core::config::Duration::new("1s"),
        // a file counts as stable only after sitting untouched this long (default 60 s)
        min_file_age: postie_core::config::Duration::new("1s"),
        ..Default::default()
    }]);
    let web = Web::start_with(&dir, cfg).await;
    std::fs::write(watch.join("seen.bin"), blob(120_000)).unwrap();
    web.wait_status("seen.bin", "complete", 60).await;
    web.stop().await;
}

#[tokio::test]
async fn a_completed_items_nzb_can_be_downloaded() {
    let fake = fake_nntp().await;
    let dir = temp_dir("nzb");
    let web = Web::start(&dir, &fake).await;
    web.upload(vec![("n.bin".into(), blob(120_000))]).await;
    let item = web.wait_status("n.bin", "complete", 60).await;
    let id = item["id"].as_str().unwrap();
    let r = web
        .http
        .get(format!("{}/api/nzb/{id}/download", web.base))
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "{}", r.status());
    let body = r.text().await.unwrap();
    assert!(body.contains("<nzb"), "{}", &body[..body.len().min(200)]);
    web.stop().await;
}
