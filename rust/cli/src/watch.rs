//! `postie watch`: queue + processor + watcher against the configured database.

use std::sync::Arc;
use std::time::Duration;

use postie_core::config::DatabaseConfig;
use postie_core::database::{self, MigrationRunner};
use postie_core::logx;
use postie_core::pool::Manager;
use postie_core::processor::runner::{PoolManager, Processor, ProcessorOptions};
use postie_core::queue::Queue;
use postie_core::watcher::{ProcessorInterface, Watcher};
use tokio_util::sync::CancellationToken;

use crate::args::Args;

fn ensure_dir(path: &str, what: &str) -> Result<(), String> {
    std::fs::create_dir_all(path)
        .map_err(|e| format!("failed to create {what} directory: {e}, {path}"))
}

pub async fn run(a: &Args) -> Result<(), String> {
    let cfg = Arc::new(crate::load(&a.config));
    let wcfg = cfg.watcher_config();
    let watch_dir = if a.dir.is_empty() { "./watch" } else { &a.dir };
    let output = if a.output_dir.is_empty() {
        "./output"
    } else {
        &a.output_dir
    };
    ensure_dir(watch_dir, "watch")?;
    ensure_dir(output, "output")?;

    let pool = Arc::new(Manager::new(&cfg)?);
    let DatabaseConfig {
        database_type,
        database_path,
    } = cfg.database.clone();
    let db = database::open(&database_type, &database_path)
        .map_err(|e| format!("failed to create database: {e}"))?;
    MigrationRunner::new(db.clone())
        .ensure_migration_compatibility()
        .map_err(|e| format!("failed to run database migrations: {e}"))?;
    let queue = Queue::new(db);

    let proc = Processor::new(ProcessorOptions {
        queue: queue.clone(),
        config: cfg.clone(),
        queue_config: cfg.queue.clone(),
        pool: Some(pool.clone() as Arc<dyn PoolManager>),
        output_folder: output.into(),
        delete_original_file: wcfg.delete_original_file,
        delete_delay: Duration::from_nanos(
            u64::try_from(wcfg.min_file_age_to_delete.nanos()).unwrap_or(0),
        ),
        maintain_original_extension: cfg.maintain_original_extension(),
        watch_folder: wcfg.watch_directory.clone(),
        follow_symlinks: wcfg.follow_symlinks,
        can_process_next_item: None,
        on_job_error: None,
        on_job_complete: None,
    });
    let cancel = CancellationToken::new();
    let (p2, c2) = (proc.clone(), cancel.clone());
    tokio::spawn(async move { p2.start(&c2).await });

    let par2_cfg = cfg.clone();
    let watcher = Arc::new(
        Watcher::new(
            wcfg,
            Arc::new(queue),
            Some(proc.clone() as Arc<dyn ProcessorInterface>),
            watch_dir,
        )
        .with_par2_config(Arc::new(move || Some(par2_cfg.par2.clone()))),
    );
    tokio::spawn(watcher.start(cancel.clone()));

    logx::info(
        "Watching directory",
        &[("dir", watch_dir.into()), ("output", output.into())],
    );
    wait_signal().await;
    logx::info("Shutting down...", &[]);
    cancel.cancel();
    proc.close().await;
    pool.close();
    Ok(())
}

async fn wait_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        if let Ok(mut term) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! { _ = ctrl_c => {}, _ = term.recv() => {} }
            return;
        }
    }
    let _ = ctrl_c.await;
}
