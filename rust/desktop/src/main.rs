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

/// Name of the DOM event the page listens to for native file drags (`detail.over`: bool).
const NATIVE_DRAG_EVENT: &str = "postie:native-drag";

/// Whether a native drag event should show (`Some(true)`) or hide (`Some(false)`) the page's drop
/// overlay; `None` for moves that change nothing.
fn drag_overlay_state(event: &tauri::DragDropEvent) -> Option<bool> {
    match event {
        tauri::DragDropEvent::Enter { paths, .. } => Some(!paths.is_empty()),
        tauri::DragDropEvent::Over { .. } => None,
        tauri::DragDropEvent::Drop { .. } | tauri::DragDropEvent::Leave => Some(false),
        _ => None,
    }
}

fn native_drag_script(over: bool) -> String {
    format!(
        "window.dispatchEvent(new CustomEvent({NATIVE_DRAG_EVENT:?}, {{ detail: {{ over: {over} }} }}))"
    )
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
            commands::select_files,
            commands::add_files,
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
            let page = window.clone();
            window.on_window_event(move |event| {
                let tauri::WindowEvent::DragDrop(drag) = event else {
                    return;
                };
                // the native handler swallows the page's own drag events, so tell the page when a
                // file drag is over the window to show its drop overlay
                if let Some(over) = drag_overlay_state(drag) {
                    let _ = page.eval(native_drag_script(over));
                }
                if let tauri::DragDropEvent::Drop { paths, .. } = drag {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_follows_the_native_drag_lifecycle() {
        let pos = tauri::PhysicalPosition::new(0.0, 0.0);
        let file = vec![PathBuf::from("/tmp/a.bin")];
        let enter = |paths| tauri::DragDropEvent::Enter {
            paths,
            position: pos,
        };
        assert_eq!(drag_overlay_state(&enter(file.clone())), Some(true));
        assert_eq!(
            drag_overlay_state(&enter(vec![])),
            Some(false),
            "no files, no overlay"
        );
        assert_eq!(
            drag_overlay_state(&tauri::DragDropEvent::Over { position: pos }),
            None
        );
        assert_eq!(
            drag_overlay_state(&tauri::DragDropEvent::Leave),
            Some(false)
        );
        let drop = tauri::DragDropEvent::Drop {
            paths: file,
            position: pos,
        };
        assert_eq!(drag_overlay_state(&drop), Some(false));
    }

    #[test]
    fn script_dispatches_the_event_the_page_listens_for() {
        assert_eq!(
            native_drag_script(true),
            r#"window.dispatchEvent(new CustomEvent("postie:native-drag", { detail: { over: true } }))"#
        );
    }
}
