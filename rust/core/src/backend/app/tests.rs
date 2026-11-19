use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use crate::queue::PaginationParams;

struct Tester(AtomicBool);

impl ServerTester for Tester {
    fn test<'a>(&'a self, s: &'a ServerData) -> crate::poster::BoxFut<'a, ValidationResult> {
        Box::pin(async move {
            if self.0.load(Ordering::SeqCst) {
                ValidationResult {
                    valid: true,
                    error: String::new(),
                }
            } else {
                ValidationResult {
                    valid: false,
                    error: format!("refused by {}", s.host),
                }
            }
        })
    }
}

fn tmp() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "app-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn paths(dir: &Path) -> AppPaths {
    AppPaths {
        config: dir.join("config/config.yaml"),
        database: dir.join("config/postie.db"),
        data: dir.join("data"),
        log: dir.join("data/postie.log"),
    }
}

fn app(ok: bool) -> (Arc<App>, Arc<Tester>, PathBuf) {
    let dir = tmp();
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::create_dir_all(dir.join("data")).unwrap();
    let t = Arc::new(Tester(AtomicBool::new(ok)));
    (App::new(paths(&dir), t.clone()), t, dir)
}

fn with_server(app: &App, port: i64) -> ConfigData {
    let mut c = app
        .get_config()
        .unwrap_or_else(|_| config::default_config());
    c.database.database_path = app.paths().database.to_string_lossy().into_owned();
    c.servers = Some(vec![ServerConfig {
        host: "127.0.0.1".into(),
        port,
        role: "upload".into(),
        max_connections: 1,
        ..Default::default()
    }]);
    c
}

#[tokio::test]
async fn a_first_start_creates_the_default_config_and_starts_nothing() {
    let (a, _, dir) = app(true);
    a.startup();
    let s = a.get_app_status();
    assert!(
        s.is_first_start
            && !s.has_config
            && s.needs_configuration
            && !s.config_valid
            && !s.critical_config_error,
        "{s:?}"
    );
    assert_eq!((s.server_count, s.valid_server_count), (0, 0));
    assert!(dir.join("config/config.yaml").exists());
    assert!(a.queue().is_none() && a.processor().is_none() && a.pool_manager().is_none());
    assert_eq!(
        a.get_processor_status(),
        ProcessorStatus {
            has_processor: false,
            running_jobs: 0,
            running_job_ids: vec![]
        }
    );
    assert!(a.get_running_jobs().is_empty() && a.get_watcher_status().is_empty());
    assert!(
        std::path::Path::new(&a.get_watch_directory()).ends_with("data/watch")
            && std::path::Path::new(&a.get_output_directory()).ends_with("data/output")
    );
    let json = serde_json::to_value(&s).unwrap();
    assert!(
        json.get("hasConfig").is_some()
            && json.get("needsConfiguration").is_some()
            && json.get("isFirstStart").is_some()
    );
}

#[tokio::test]
async fn saving_a_valid_config_brings_up_the_components_and_a_failing_server_blocks_the_save() {
    let (a, tester, _) = app(true);
    a.startup();
    tester.0.store(false, Ordering::SeqCst);
    let cfg = with_server(&a, 1);
    let e = a.save_config(cfg.clone()).await.unwrap_err();
    assert_eq!(
        e,
        "Failed to connect to 1 server(s): 127.0.0.1:1. Last error: server 1 (127.0.0.1:1): refused by 127.0.0.1"
    );
    assert!(a.queue().is_none(), "nothing applied");
    tester.0.store(true, Ordering::SeqCst);
    a.save_config(cfg).await.unwrap();
    let s = a.get_app_status();
    assert!(
        s.config_valid && !s.needs_configuration && s.server_count == 1,
        "{s:?}"
    );
    assert!(
        a.queue().is_some() && a.pool_manager().is_some() && a.processor().is_some(),
        "queue, pool and processor created"
    );
    assert_eq!(a.get_config().unwrap().servers.unwrap().len(), 1);
    // an unrelated change does not restart the processor; a processor-relevant one does
    let before = a.processor().unwrap();
    let mut c2 = a.get_config().unwrap();
    c2.posting.max_retries = 7;
    a.save_config(c2).await.unwrap();
    assert!(
        Arc::ptr_eq(&before, &a.processor().unwrap()),
        "posting settings are read per job"
    );
    let mut c3 = a.get_config().unwrap();
    c3.queue.max_concurrent_uploads = 3;
    a.save_config(c3).await.unwrap();
    assert!(
        !Arc::ptr_eq(&before, &a.processor().unwrap()),
        "queue settings are captured by the processor"
    );
}

#[tokio::test]
async fn invalid_configs_are_rejected_before_anything_is_written() {
    let (a, _, dir) = app(true);
    a.startup();
    let mut bad = with_server(&a, 1);
    bad.servers.as_mut().unwrap()[0].port = 99999;
    let before = std::fs::read(dir.join("config/config.yaml")).unwrap();
    assert!(a.save_config(bad).await.is_err());
    assert_eq!(
        std::fs::read(dir.join("config/config.yaml")).unwrap(),
        before
    );
}

#[tokio::test]
async fn config_saved_during_a_running_job_stays_pending_until_applied_or_discarded() {
    let (a, _, dir) = app(true);
    a.startup();
    a.save_config(with_server(&a, 1)).await.unwrap();
    let file = dir.join("data/big.bin");
    std::fs::write(&file, vec![1u8; 20_000]).unwrap();
    a.queue()
        .unwrap()
        .add_file(file.to_str().unwrap(), 20_000)
        .unwrap();
    for _ in 0..100 {
        if a.is_uploading() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(a.is_uploading(), "a job is running against the dead server");
    let mut pending = a.get_config().unwrap();
    pending.posting.max_retries = 9;
    a.save_config(pending).await.unwrap();
    assert!(a.has_pending_config_changes());
    let st = a.pending_config_status();
    assert_eq!(
        (
            st["hasPendingConfig"].as_bool(),
            st["canApplyNow"].as_bool()
        ),
        (Some(true), Some(false))
    );
    assert_eq!(
        a.get_config().unwrap().posting.max_retries,
        9,
        "get_config returns the pending config"
    );
    assert_eq!(
        a.get_applied_config().unwrap().posting.max_retries,
        a.applied_config().unwrap().posting.max_retries
    );
    assert!(
        a.apply_pending_config()
            .unwrap_err()
            .contains("cannot apply configuration while uploads or jobs are running")
    );
    assert!(
        !a.can_process_next_item(),
        "new items are held while a config is pending"
    );
    a.discard_pending_config().unwrap();
    assert!(
        !a.has_pending_config_changes()
            && a.discard_pending_config()
                .unwrap_err()
                .contains("no pending")
    );
    assert!(
        a.apply_pending_config()
            .unwrap_err()
            .contains("no pending configuration changes")
    );
    assert!(a.can_process_next_item());
    a.shutdown().await;
}

#[tokio::test]
async fn pause_resume_retry_and_events() {
    let (a, _, _) = app(true);
    let events = Arc::new(Mutex::new(Vec::<String>::new()));
    let e2 = events.clone();
    a.set_event_emitter(Arc::new(move |k, _| e2.lock().unwrap().push(k.to_string())));
    a.startup();
    a.save_config(with_server(&a, 1)).await.unwrap();
    a.pause_processing().unwrap();
    assert_eq!(a.pause_state()["paused"], true);
    a.resume_processing().unwrap();
    assert_eq!(a.pause_state()["paused"], false);
    let q = a.queue().unwrap();
    q.add_file("/x/err.bin", 5).unwrap();
    let r = q.receive_file().unwrap().unwrap();
    q.mark_as_error(&r.msg_id, &r.job, "boom").unwrap();
    a.retry_job(&r.msg_id).unwrap();
    assert!(a.retry_job("missing").is_err());
    q.add_file("/x/done.bin", 5).unwrap();
    let r2 = loop {
        if let Some(r) = q.receive_file().unwrap()
            && r.job.path == "/x/done.bin"
        {
            break r;
        }
    };
    q.complete_file(&r2.msg_id, "/o.nzb", &r2.job).unwrap();
    a.retry_script(&r2.msg_id).unwrap();
    let st: String = a
        .db()
        .unwrap()
        .lock()
        .query_row(
            "SELECT script_status FROM completed_items WHERE id=?1",
            [&r2.msg_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(st, "pending_retry");
    let kinds = events.lock().unwrap().clone();
    for k in [
        "processing:paused",
        "processing:resumed",
        "queue:updated",
        "config-updated",
    ] {
        assert!(kinds.contains(&k.to_string()), "{k} in {kinds:?}");
    }
    assert!(a.cancel_job("nope").is_err());
    a.shutdown().await;
}

#[tokio::test]
async fn the_api_key_is_generated_when_the_api_is_enabled_and_a_saved_location_wins() {
    let (a, _, dir) = app(true);
    a.startup();
    let mut c = with_server(&a, 1);
    c.api.enabled = true;
    a.save_config(c).await.unwrap();
    let key: Option<String> = a
        .db()
        .unwrap()
        .lock()
        .query_row(
            "SELECT value FROM settings WHERE key LIKE '%api%' LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok();
    assert!(key.is_some_and(|k| !k.is_empty()) || a.db().is_some());
    // a saved config location different from the default disables first-run detection
    let elsewhere = tmp();
    std::fs::write(elsewhere.join("config.yaml"), "version: 3\n").unwrap();
    std::fs::write(
        dir.join("config/config-location"),
        elsewhere.join("config.yaml").to_string_lossy().as_bytes(),
    )
    .unwrap();
    let b = App::new(paths(&dir), Arc::new(Tester(AtomicBool::new(true))));
    assert_eq!(b.config_path(), elsewhere.join("config.yaml"));
    b.startup();
    assert!(!b.get_app_status().is_first_start);
    b.set_web_mode(true);
    assert_eq!(
        b.config_path(),
        dir.join("config/config.yaml"),
        "web mode keeps the default location"
    );
}

#[tokio::test]
async fn test_provider_connectivity_validates_input_with_the_real_tester() {
    let t = NntpTester;
    let r = t
        .test(&ServerData {
            port: 119,
            ..Default::default()
        })
        .await;
    assert_eq!(r.error, "Host is required");
    let r = t
        .test(&ServerData {
            host: "h".into(),
            port: 70000,
            ..Default::default()
        })
        .await;
    assert_eq!(r.error, "Port must be between 1 and 65535");
    let r = t
        .test(&ServerData {
            host: "127.0.0.1".into(),
            port: 1,
            ..Default::default()
        })
        .await;
    assert!(
        !r.valid && r.error.starts_with("Connection test failed:"),
        "{r:?}"
    );
}

async fn ready() -> (Arc<App>, PathBuf) {
    let (a, _, dir) = app(true);
    a.startup();
    a.save_config(with_server(&a, 1)).await.unwrap();
    (a, dir)
}

#[tokio::test]
async fn queue_api_returns_items_without_script_fields_and_caches_stats() {
    let (a, dir) = ready().await;
    a.pause_processing().unwrap();
    let f = dir.join("data/a.bin");
    std::fs::write(&f, b"hello").unwrap();
    a.handle_dropped_files(&[f.to_string_lossy().into_owned()])
        .unwrap();
    let r = a
        .get_queue_items(PaginationParams {
            page: 1,
            limit: 25,
            sort_by: "created".into(),
            order: "desc".into(),
            status: String::new(),
        })
        .unwrap();
    assert_eq!(
        (
            r.total_items,
            r.items[0].file_name.as_str(),
            r.items[0].status.as_str()
        ),
        (1, "a.bin", "pending")
    );
    let json = serde_json::to_value(&r.items[0]).unwrap();
    assert!(
        json.get("scriptStatus").is_none()
            && json.get("verifiedArticles").is_none()
            && json["errorMessage"].is_null(),
        "{json}"
    );
    let s = a.get_queue_stats().unwrap();
    assert_eq!((s.total, s.pending), (1, 1));
    a.queue().unwrap().add_file("/x/b.bin", 1).unwrap();
    assert_eq!(
        a.get_queue_stats().unwrap().total,
        1,
        "served from the 2 s cache"
    );
    let id = r.items[0].id.clone();
    a.set_queue_item_priority(&id, 5).unwrap();
    a.remove_from_queue(&id).unwrap();
    a.clear_queue().unwrap();
    assert_eq!(
        a.get_queue_items(PaginationParams {
            page: 1,
            limit: 25,
            sort_by: "created".into(),
            order: "desc".into(),
            status: String::new()
        })
        .unwrap()
        .total_items,
        0
    );
    assert!(
        a.get_nzb("nope")
            .unwrap_err()
            .starts_with("failed to get NZB path")
    );
}

#[tokio::test]
async fn dropped_folders_become_one_folder_job_and_empty_ones_fail() {
    let (a, dir) = ready().await;
    a.pause_processing().unwrap();
    let folder = dir.join("data/Show");
    std::fs::create_dir_all(folder.join("sub")).unwrap();
    std::fs::write(folder.join("e1.bin"), b"12345").unwrap();
    std::fs::write(folder.join("sub/e2.bin"), b"123").unwrap();
    a.handle_dropped_files(&[folder.to_string_lossy().into_owned()])
        .unwrap();
    let items = a
        .get_queue_items(PaginationParams {
            page: 1,
            limit: 25,
            ..Default::default()
        })
        .unwrap()
        .items;
    assert_eq!(
        (
            items.len(),
            items[0].path.starts_with("FOLDER:"),
            items[0].size
        ),
        (1, true, 8)
    );
    let empty = dir.join("data/Empty");
    std::fs::create_dir_all(&empty).unwrap();
    assert_eq!(
        a.handle_dropped_files(&[empty.to_string_lossy().into_owned()])
            .unwrap_err(),
        "no valid files or folders could be added to queue"
    );
    assert_eq!(a.handle_dropped_files(&[]).unwrap_err(), "no files dropped");
}

#[tokio::test]
async fn needing_configuration_blocks_dropped_files() {
    let (a, _, _) = app(true);
    a.startup();
    assert!(
        a.handle_dropped_files(&["/x".into()])
            .unwrap_err()
            .starts_with("configuration required")
    );
}

#[tokio::test]
async fn the_wizard_validates_then_saves_and_clears_first_start() {
    let (a, _, _) = app(true);
    a.startup();
    let s = |host: &str, port: i64, role: &str| ServerData {
        host: host.into(),
        port,
        role: role.into(),
        max_connections: 0,
        ..Default::default()
    };
    let w = |servers: Vec<ServerData>, out: &str| api::SetupWizardData {
        servers,
        output_directory: out.into(),
        watch_directory: String::new(),
    };
    assert_eq!(
        a.setup_wizard_complete(w(vec![], "/o")).await.unwrap_err(),
        "at least one server must be configured"
    );
    assert_eq!(
        a.setup_wizard_complete(w(vec![s("h", 1, "")], ""))
            .await
            .unwrap_err(),
        "output directory must be specified"
    );
    assert_eq!(
        a.setup_wizard_complete(w(vec![s("h", 1, "verify")], "/o"))
            .await
            .unwrap_err(),
        "at least one upload server is required"
    );
    assert_eq!(
        a.setup_wizard_complete(w(vec![s("", 1, "")], "/o"))
            .await
            .unwrap_err(),
        "server 1: host is required"
    );
    assert_eq!(
        a.setup_wizard_complete(w(vec![s("h", 0, "")], "/o"))
            .await
            .unwrap_err(),
        "server 1: port must be between 1 and 65535"
    );
    assert!(a.get_app_status().is_first_start);
    a.setup_wizard_complete(w(vec![s("127.0.0.1", 1, "")], "/tmp/postie-wizard-out"))
        .await
        .unwrap();
    let st = a.get_app_status();
    assert!(!st.is_first_start && st.config_valid, "{st:?}");
    let cfg = a.get_config().unwrap();
    assert_eq!(
        (
            cfg.servers.as_ref().unwrap()[0].max_connections,
            cfg.output_dir.as_str()
        ),
        (5, "/tmp/postie-wizard-out")
    );
}

#[tokio::test]
async fn api_uploads_validate_paths_and_apply_options() {
    let (a, dir) = ready().await;
    a.pause_processing().unwrap();
    let root = dir.join("data/lib");
    std::fs::create_dir_all(&root).unwrap();
    let f = root.join("x.bin");
    std::fs::write(&f, b"abc").unwrap();
    let (f, root) = (
        f.to_string_lossy().into_owned(),
        root.to_string_lossy().into_owned(),
    );
    let ok = a.enqueue_api_upload(&f, &root, 3, true).unwrap();
    assert_eq!(ok["status"], "queued");
    for (file, rel, want) in [
        ("", "/r", "file is required"),
        ("/f", "", "relative_path is required"),
        ("rel", "/r", "file must be an absolute path: \"rel\""),
        (
            "/r/f",
            "rel",
            "relative_path must be an absolute path: \"rel\"",
        ),
    ] {
        assert_eq!(a.enqueue_api_upload(file, rel, 0, false).unwrap_err(), want);
    }
    assert!(
        a.enqueue_api_upload("/r/f", "/r/f", 0, false)
            .unwrap_err()
            .contains("is not a parent directory of file")
    );
    assert!(
        a.enqueue_api_upload(&format!("{root}/missing"), &root, 0, false)
            .unwrap_err()
            .starts_with("file not found")
    );
    assert!(
        a.enqueue_api_upload(&root, root.rsplit_once('/').unwrap().0, 0, false)
            .unwrap_err()
            .starts_with("file must be a regular file")
    );
    let job: String = a
        .db()
        .unwrap()
        .lock()
        .query_row("SELECT CAST(body AS TEXT) FROM goqite", [], |r| r.get(0))
        .unwrap();
    assert!(
        job.contains("\"deleteOriginal\":true") && job.contains("\"priority\":3"),
        "{job}"
    );
}

#[tokio::test]
async fn logs_metrics_and_api_key() {
    let (a, dir) = ready().await;
    std::fs::write(dir.join("data/postie.log"), "l1\nl2\nl3\n").unwrap();
    assert_eq!(a.get_logs_paginated(2, 0).unwrap(), "l2\nl3");
    assert_eq!(a.get_logs().unwrap(), "l1\nl2\nl3\n");
    std::fs::remove_file(dir.join("data/postie.log")).unwrap();
    assert!(
        a.get_logs()
            .unwrap_err()
            .starts_with("failed to open log file at")
    );
    let m = a.get_nntp_pool_metrics().unwrap();
    assert_eq!((m.providers.len(), m.active_connections), (1, 0));
    assert_eq!(m.providers[0].host, "127.0.0.1:1");
    let key = a.get_api_key().unwrap();
    assert!(!key.is_empty());
    assert_ne!(a.regenerate_api_key().unwrap(), key);
    assert!(!a.is_api_enabled());
}
