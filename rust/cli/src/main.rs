//! `postie` command line: upload a directory or file once, or `postie watch` a directory.

mod args;
mod files;
mod watch;

use std::path::Path;
use std::sync::Arc;

use postie_core::config::{self, ConfigData};
use postie_core::logx;
use postie_core::pool::Manager;
use postie_core::poster::ArticleClient;
use postie_core::postie::Postie;
use postie_core::progress::JobProgress;
use tokio_util::sync::CancellationToken;

fn fail(e: impl std::fmt::Display) -> ! {
    logx::error("Command failed", &[("error", e.to_string().into())]);
    eprintln!("Error: {e}");
    std::process::exit(1);
}

fn load(path: &str) -> ConfigData {
    config::load(Path::new(path)).unwrap_or_else(|e| fail(e))
}

async fn upload(a: &args::Args) -> Result<(), String> {
    let (files, root) = if a.input_file.is_empty() {
        (
            files::collect_directory(&a.dir, &a.ignore_patterns)?,
            a.dir.clone(),
        )
    } else {
        files::single(&a.input_file)?
    };
    if files.is_empty() {
        return Err("no files to post".into());
    }
    let cfg = load(&a.config);
    let pool = Manager::new(&cfg)?;
    let upload = pool.upload_pool().ok_or("no upload servers configured")?;
    let upload: Arc<dyn ArticleClient> = upload;
    let verify: Arc<dyn ArticleClient> = pool
        .verify_pool()
        .map_or_else(|| upload.clone(), |c| c as Arc<dyn ArticleClient>);
    let progress = Arc::new(JobProgress::new("postie-job"));
    let postie = Postie::new(&cfg, upload, verify, progress, None, None, "", None);
    logx::info(
        "Starting upload from directory",
        &[("dir", root.clone().into())],
    );
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        c2.cancel();
    });
    let res = postie
        .post(
            &cancel,
            &files,
            &root,
            &a.output_dir,
            a.input_file.is_empty(),
        )
        .await;
    pool.close();
    res.map(|_| ()).map_err(|e| e.to_string())
}

#[tokio::main]
async fn main() {
    let a = args::parse(std::env::args().skip(1)).unwrap_or_else(|e| fail(e));
    if a.help {
        print!("{}", args::USAGE);
        return;
    }
    if a.version {
        println!("postie {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    let res = if a.watch {
        watch::run(&a).await
    } else {
        upload(&a).await
    };
    if let Err(e) = res {
        fail(e);
    }
}
