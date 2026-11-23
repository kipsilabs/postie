//! The Logs page reads the log file: an upload's life (queued, posted, verified) must reach it.
//! Alone in its binary because the log sink is process-wide.

mod support;

use std::path::Path;

use support::*;

#[tokio::test]
async fn an_upload_logs_when_it_is_queued_posted_and_verified() {
    let fake = fake_nntp().await;
    let dir = temp_dir("log-activity");
    let log = dir.join("data/postie.log");
    postie_core::logx::setup(&log).expect("log setup");
    let web = Web::start(&dir, &fake).await;
    let src = dir.join("src/logged.bin");
    std::fs::create_dir_all(src.parent().unwrap()).unwrap();
    std::fs::write(&src, blob(300_000)).unwrap();
    web.app
        .handle_dropped_files(&[src.to_string_lossy().into_owned()])
        .expect("add file");
    let wanted = [
        "Added to the upload queue",
        "Posting file",
        "Verifying upload",
        "Upload verified",
    ];
    wait("lifecycle lines in the log", 90, || async {
        let text = std::fs::read_to_string(Path::new(&log)).unwrap_or_default();
        wanted.iter().all(|w| text.contains(w)).then_some(())
    })
    .await;
    let served = web
        .http
        .get(format!("{}/api/logs", web.base))
        .send()
        .await
        .unwrap();
    let body = served.text().await.unwrap();
    for w in wanted {
        assert!(body.contains(w), "the logs API serves {w:?}");
    }
    web.stop().await;
}
