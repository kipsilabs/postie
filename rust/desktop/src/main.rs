//! Postie desktop (Tauri v2): the web server runs in-process on a loopback port and a native window
//! shows it, so the desktop and web builds share one backend and one frontend. Desktop mode keeps the
//! saved configuration location (the web server pins the default one).

mod commands;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use postie_core::backend::app::{App, NntpTester};
use postie_core::backend::paths::{AppPaths, get_app_paths};
use postie_web::Srv;
use tauri::{Manager, RunEvent, WebviewUrl, WebviewWindowBuilder};

/// The frontend build: the bundled resource, else `POSTIE_FRONTEND_DIR`, else the source tree's build.
fn frontend_dir(app: &tauri::AppHandle) -> PathBuf {
    if let Some(d) = std::env::var_os("POSTIE_FRONTEND_DIR") {
        return PathBuf::from(d);
    }
    if let Ok(res) = app.path().resource_dir() {
        let bundled = res.join("frontend");
        if bundled.join("index.html").exists() {
            return bundled;
        }
    }
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../frontend/build"))
}

struct Backend {
    app: Arc<App>,
    shutdown: tokio_util::sync::CancellationToken,
}

fn main() {
    let paths = get_app_paths().unwrap_or_else(|_| AppPaths {
        config: "./config.yaml".into(),
        database: "./postie.db".into(),
        data: ".".into(),
        log: "./postie.log".into(),
    });
    if let Err(e) = postie_core::logx::setup(&paths.log) {
        postie_core::logx::error(
            "Failed to setup file logging, using stdout only",
            &[("error", e.into())],
        );
    }
    let backend: Arc<Mutex<Option<Backend>>> = Arc::new(Mutex::new(None));
    let b2 = backend.clone();
    let tauri_app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![
            commands::select_folder,
            commands::select_folders,
            commands::upload_folders,
            commands::select_config_directory,
            commands::download_nzb,
            commands::get_config_path,
        ])
        .setup(move |tauri_app| {
            let dir = frontend_dir(tauri_app.handle());
            let (app, running) = tauri::async_runtime::block_on(async {
                let app = App::new(paths, Arc::new(NntpTester));
                app.set_web_mode(false);
                app.startup();
                let srv = Srv::new(app.clone(), None, false, dir);
                let running = postie_web::serve(srv, "127.0.0.1:0")
                    .await
                    .map_err(|e| e.to_string())?;
                Ok::<_, String>((app, running))
            })?;
            let url = format!("http://{}/", running.addr);
            postie_core::logx::info(
                "Desktop server started",
                &[("addr", running.addr.to_string().into())],
            );
            tauri_app.manage(app.clone());
            let app_for_drop = app.clone();
            *b2.lock().unwrap_or_else(|p| p.into_inner()) = Some(Backend {
                app,
                shutdown: running.shutdown,
            });
            let parsed = url.parse().map_err(|e: url::ParseError| e.to_string())?;
            let window = WebviewWindowBuilder::new(tauri_app, "main", WebviewUrl::External(parsed))
                .title("Postie")
                .inner_size(1280.0, 820.0)
                .min_inner_size(900.0, 600.0)
                .build()?;
            // files dropped on the window are queued by path (no copy through HTTP)
            let dropped = app_for_drop;
            window.on_window_event(move |event| {
                if let tauri::WindowEvent::DragDrop(tauri::DragDropEvent::Drop { paths, .. }) =
                    event
                {
                    let paths: Vec<String> = paths
                        .iter()
                        .map(|p| p.to_string_lossy().into_owned())
                        .collect();
                    let app = dropped.clone();
                    std::thread::spawn(move || {
                        if let Err(e) = app.handle_dropped_files(&paths) {
                            postie_core::logx::error(
                                "Dropped files rejected",
                                &[("error", e.into())],
                            );
                        }
                    });
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the Postie desktop app");
    tauri_app.run(move |_handle, event| {
        if let RunEvent::Exit = event
            && let Some(b) = backend.lock().unwrap_or_else(|p| p.into_inner()).take()
        {
            b.shutdown.cancel();
            tauri::async_runtime::block_on(b.app.shutdown());
        }
    });
}
