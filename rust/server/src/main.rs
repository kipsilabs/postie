//! `postie-web`: flags `--auth`, `--port/-p`, `--host`; env `POSTIE_AUTH_ENABLED`, `PORT`, `HOST`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use postie_core::backend::app::{App, NntpTester};
use postie_core::backend::paths::get_app_paths;
use postie_core::config::DatabaseConfig;
use postie_core::database;
use postie_core::webauth;
use postie_web::Srv;

fn parse_bool(s: &str) -> Result<bool, String> {
    match s {
        "1" | "t" | "T" | "TRUE" | "true" | "True" => Ok(true),
        "0" | "f" | "F" | "FALSE" | "false" | "False" => Ok(false),
        other => Err(format!(
            "strconv.ParseBool: parsing {other:?}: invalid syntax"
        )),
    }
}

struct Args {
    auth: Option<bool>,
    port: String,
    host: String,
}

fn parse_args() -> Result<Args, String> {
    let mut a = Args {
        auth: None,
        port: std::env::var("PORT")
            .ok()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| "8080".into()),
        host: std::env::var("HOST")
            .ok()
            .filter(|p| !p.is_empty())
            .unwrap_or_else(|| "0.0.0.0".into()),
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--auth" => a.auth = Some(true),
            "--port" | "-p" => a.port = it.next().ok_or("flag needs an argument: --port")?,
            "--host" => a.host = it.next().ok_or("flag needs an argument: --host")?,
            s if s.starts_with("--port=") => a.port = s["--port=".len()..].into(),
            s if s.starts_with("--host=") => a.host = s["--host=".len()..].into(),
            s if s.starts_with("--auth=") => a.auth = Some(parse_bool(&s["--auth=".len()..])?),
            other => return Err(format!("unknown flag: {other}")),
        }
    }
    Ok(a)
}

async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! { _ = ctrl_c => {}, _ = term.recv() => {} }
    }
    #[cfg(not(unix))]
    {
        let _ = ctrl_c.await;
    }
}

/// `POSTIE_FRONTEND_DIR`, else a `frontend/` directory next to the executable (release archives),
/// else the source tree's build output (found from the crate location, then from the working directory).
fn frontend_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("POSTIE_FRONTEND_DIR") {
        return PathBuf::from(d);
    }
    let beside = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|p| p.join("frontend")));
    let candidates = [
        beside,
        Some(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../frontend/build")),
        Some(PathBuf::from("frontend/build")),
        Some(PathBuf::from("../frontend/build")),
    ];
    let dir = candidates
        .into_iter()
        .flatten()
        .find(|p| p.join("index.html").is_file());
    dir.unwrap_or_else(|| {
        eprintln!(
            "warning: frontend build not found; run `bun run build` in frontend/ or set POSTIE_FRONTEND_DIR"
        );
        PathBuf::from("frontend/build")
    })
}

#[tokio::main]
async fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("Fatal error: {e}");
            std::process::exit(1);
        }
    };
    // an invalid environment value must fail startup rather than disable protection
    let auth_enabled = match (args.auth, std::env::var("POSTIE_AUTH_ENABLED")) {
        (Some(v), _) => v,
        (None, Ok(v)) => match parse_bool(&v) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("Fatal error: POSTIE_AUTH_ENABLED: {e}");
                std::process::exit(1);
            }
        },
        (None, Err(_)) => false,
    };
    let paths = get_app_paths().unwrap_or_else(|e| {
        eprintln!("could not get OS-specific paths, using current directory: {e}");
        postie_core::backend::paths::AppPaths {
            config: "./config.yaml".into(),
            database: "./postie.db".into(),
            data: ".".into(),
            log: "./postie.log".into(),
        }
    });
    if let Err(e) = postie_core::logx::setup(&paths.log) {
        postie_core::logx::error(
            "Failed to setup file logging, using stdout only",
            &[("error", e.into())],
        );
    }
    let db_path = paths.database.clone();
    let app = App::new(paths, Arc::new(NntpTester));
    let build_dir = frontend_dir();
    app.set_web_mode(true);
    app.startup();
    // authentication must work before NNTP setup and outlive posting queue restarts: it owns a separate
    // connection to the same application database
    let auth = if auth_enabled {
        let db = database::open(
            &DatabaseConfig {
                database_type: "sqlite".into(),
                database_path: db_path.to_string_lossy().into_owned(),
            }
            .database_type,
            &db_path.to_string_lossy(),
        )
        .unwrap_or_else(|e| {
            eprintln!("Fatal error: authentication database: {e}");
            std::process::exit(1);
        });
        if let Err(e) = database::MigrationRunner::new(db.clone()).ensure_migration_compatibility()
        {
            eprintln!("Fatal error: authentication migrations: {e}");
            std::process::exit(1);
        }
        match webauth::Service::new(db) {
            Ok(s) => Some(Arc::new(s)),
            Err(e) => {
                eprintln!("Fatal error: initialize web authentication: {e}");
                std::process::exit(1);
            }
        }
    } else {
        None
    };
    let srv = Srv::new(app.clone(), auth, auth_enabled, build_dir);
    let addr = format!("{}:{}", args.host, args.port);
    let running = match postie_web::serve(srv, &addr).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("Fatal error: failed to start web server: {e}");
            std::process::exit(1);
        }
    };
    postie_core::logx::info("Web server started", &[("addr", addr.clone().into())]);
    shutdown_signal().await;
    running.shutdown.cancel();
    let _ = running.task.await;
    postie_core::logx::info("Received interrupt signal, shutting down gracefully", &[]);
    app.shutdown().await;
    // everything is stopped and flushed: do not let the runtime's drop wait on a straggling blocking
    // task (a scan or a connect that is still in flight) and keep the process alive after Ctrl-C
    std::process::exit(0);
}
