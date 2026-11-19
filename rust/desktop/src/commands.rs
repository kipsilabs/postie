//! Native-only operations exposed to the page (folder pickers, NZB save, config relocation).
//! The page reaches them through `window.__TAURI_INTERNALS__`, allowed only for the loopback origin
//! this app serves (see `capabilities/default.json`).

use std::sync::Arc;

use postie_core::backend::app::App;
use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

fn path_string(p: tauri_plugin_dialog::FilePath) -> Option<String> {
    p.into_path().ok().map(|p| p.to_string_lossy().into_owned())
}

#[tauri::command]
pub async fn select_folder(app: AppHandle, title: Option<String>) -> Option<String> {
    let mut d = app.dialog().file();
    if let Some(t) = title {
        d = d.set_title(t);
    }
    d.blocking_pick_folder().and_then(path_string)
}

#[tauri::command]
pub async fn select_folders(app: AppHandle) -> Vec<String> {
    app.dialog()
        .file()
        .set_title("Select folders to upload")
        .blocking_pick_folders()
        .map(|v| v.into_iter().filter_map(path_string).collect())
        .unwrap_or_default()
}

/// Queues folders picked natively (each becomes its own NZB).
#[tauri::command]
pub async fn upload_folders(app: State<'_, Arc<App>>, paths: Vec<String>) -> Result<(), String> {
    if paths.is_empty() {
        return Err("no folder paths provided".into());
    }
    let app = app.inner().clone();
    tokio::task::spawn_blocking(move || {
        let mut last = Ok(());
        for p in paths {
            last = app.handle_dropped_files(&[p]);
        }
        last
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Copies the saved configuration to a chosen directory; empty string when cancelled.
#[tauri::command]
pub async fn select_config_directory(
    handle: AppHandle,
    app: State<'_, Arc<App>>,
) -> Result<String, String> {
    let mut d = handle
        .dialog()
        .file()
        .set_title("Choose configuration directory");
    if let Some(dir) = app.config_path().parent() {
        d = d.set_directory(dir);
    }
    let Some(dir) = d.blocking_pick_folder().and_then(path_string) else {
        return Ok(String::new());
    };
    app.relocate_config(&dir)
}

/// Saves a completed item's NZB where the user chooses; cancelling is not an error.
#[tauri::command]
pub async fn download_nzb(
    handle: AppHandle,
    app: State<'_, Arc<App>>,
    id: String,
) -> Result<(), String> {
    let app = app.inner().clone();
    let (bytes, name) = tokio::task::spawn_blocking(move || app.get_nzb(&id))
        .await
        .map_err(|e| e.to_string())??;
    let Some(dest) = handle
        .dialog()
        .file()
        .set_title("Save NZB File")
        .set_file_name(name)
        .blocking_save_file()
        .and_then(path_string)
    else {
        return Ok(());
    };
    std::fs::write(&dest, bytes).map_err(|e| format!("failed to write NZB file: {e}"))
}

#[tauri::command]
pub fn get_config_path(app: State<'_, Arc<App>>) -> String {
    app.config_path().to_string_lossy().into_owned()
}
