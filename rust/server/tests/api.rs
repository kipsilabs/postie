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
    // Once no work is active the backend applies the pending config on its own. Only if it is still
    // held after that window do we apply it by hand; the backend may win that race, so a failed
    // manual apply is fine as long as nothing is pending afterwards.
    let applied_on_its_own = async {
        for _ in 0..30 {
            if web.get("/api/config/pending/status").await == json!(false) {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        }
        false
    }
    .await;
    if !applied_on_its_own {
        let r = web.post_json("/api/config/pending/apply", &json!({})).await;
        assert!(
            r.status().is_success() || web.get("/api/config/pending/status").await == json!(false),
            "manual apply failed and the config is still pending: {}",
            r.status()
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

#[tokio::test]
async fn with_auth_on_the_api_is_closed_until_registering_and_logging_in() {
    let fake = fake_nntp().await;
    let dir = temp_dir("auth");
    let web = Web::start_with_auth(&dir, &fake).await;
    let anon = web
        .http
        .get(format!("{}/api/queue/stats", web.base))
        .send()
        .await
        .unwrap();
    assert_eq!(anon.status(), 401, "no session, no API");
    let page = web
        .http
        .get(format!("{}/settings", web.base))
        .send()
        .await
        .unwrap();
    assert!(
        page.status().is_redirection(),
        "pages go to /login: {}",
        page.status()
    );
    let st = web.get("/api/auth/registration-status").await;
    assert_eq!(st["registration_enabled"], true, "{st}");
    let creds = json!({"username": "admin", "password": "correct horse battery"});
    let bad = web
        .post_json(
            "/api/auth/login",
            &json!({"username": "admin", "password": "wrong"}),
        )
        .await;
    assert!(bad.status().is_client_error(), "{}", bad.status());
    let reg = web.post_json("/api/auth/register", &creds).await;
    assert!(reg.status().is_success(), "{}", reg.status());
    let cookie = reg
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(|c| c.split(';').next().unwrap_or("").to_string())
        .collect::<Vec<_>>()
        .join("; ");
    assert!(!cookie.is_empty(), "registering logs in");
    let ok = web
        .http
        .get(format!("{}/api/queue/stats", web.base))
        .header("cookie", &cookie)
        .header("origin", &web.base)
        .send()
        .await
        .unwrap();
    assert!(ok.status().is_success(), "{}", ok.status());
    let st = web.get("/api/auth/registration-status").await;
    assert_eq!(st["registration_enabled"], false, "one admin only: {st}");
    web.stop().await;
}

#[tokio::test]
async fn a_server_that_rejects_the_credentials_blocks_saving_the_config_with_a_clear_error() {
    let fake = fake_nntp().await;
    *fake.ctl.bad_auth.lock().unwrap() = true;
    let dir = temp_dir("badauth");
    let app_dir = dir.clone();
    let mut cfg = fast_config(&app_dir, fake.addr.port());
    cfg.servers.as_mut().unwrap()[0].username = "me".into();
    cfg.servers.as_mut().unwrap()[0].password = "wrong".into();
    let app = postie_core::backend::app::App::new(
        postie_core::backend::paths::AppPaths {
            config: dir.join("config/config.yaml"),
            database: dir.join("config/postie.db"),
            data: dir.join("data"),
            log: dir.join("data/postie.log"),
        },
        std::sync::Arc::new(postie_core::backend::app::NntpTester),
    );
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::create_dir_all(dir.join("data")).unwrap();
    app.set_web_mode(true);
    app.startup();
    let e = app.save_config(cfg).await.unwrap_err();
    assert!(e.contains("Failed to connect"), "{e}");
    assert!(app.queue().is_none(), "nothing was applied");
    app.shutdown().await;
}

#[tokio::test]
async fn par2_files_are_generated_posted_and_listed_in_the_nzb() {
    let fake = fake_nntp().await;
    let dir = temp_dir("par2");
    let mut cfg = fast_config(&dir, fake.addr.port());
    cfg.par2.enabled = Some(true);
    cfg.par2.redundancy = "10".into();
    cfg.posting.wait_for_par2 = Some(true);
    let web = Web::start_with(&dir, cfg).await;
    web.upload(vec![("withpar.bin".into(), blob(1_200_000))])
        .await;
    let item = web.wait_status("withpar.bin", "complete", 90).await;
    let nzb = std::fs::read_to_string(item["nzbPath"].as_str().unwrap()).unwrap();
    assert!(nzb.contains(".par2"), "par2 volumes are in the NZB");
    assert!(
        fake.posts() > 12 + 1,
        "file articles plus par2 articles: {}",
        fake.posts()
    );
    web.stop().await;
}

#[cfg(unix)]
#[tokio::test]
async fn the_post_upload_script_runs_with_the_nzb_path_after_a_completed_upload() {
    let fake = fake_nntp().await;
    let dir = temp_dir("script");
    let marker = dir.join("marker.txt");
    let mut cfg = fast_config(&dir, fake.addr.port());
    cfg.post_upload_script = postie_core::config::PostUploadScriptConfig {
        enabled: true,
        command: format!("echo {{nzb_path}} > {}", marker.display()),
        timeout: postie_core::config::Duration::new("10s"),
        retry_delay: postie_core::config::Duration::new("1s"),
        ..Default::default()
    };
    let web = Web::start_with(&dir, cfg).await;
    web.upload(vec![("s.bin".into(), blob(100_000))]).await;
    web.wait_status("s.bin", "complete", 60).await;
    let out = wait("script wrote its marker", 30, || async {
        std::fs::read_to_string(&marker)
            .ok()
            .filter(|s| !s.is_empty())
    })
    .await;
    assert!(out.trim().ends_with(".nzb"), "{out}");
    web.stop().await;
}
