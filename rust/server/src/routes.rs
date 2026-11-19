//! The route table and handlers of `cmd/web/main.go`, `api_key.go` and `arr_handlers.go`.

use std::sync::Arc;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{
    DefaultBodyLimit, Multipart, Path, Query, Request, State, WebSocketUpgrade, ws,
};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware;
use axum::response::Response;
use axum::routing::{delete, get, post};
use postie_core::backend::app::{ServerData, api::SetupWizardData};
use postie_core::config::ConfigData;
use postie_core::queue::PaginationParams;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth;
use crate::util::{
    bad_request, internal, json_response, ok_empty, ok_json, status_only, text_error,
};
use crate::{Srv, static_files};

type S = State<Arc<Srv>>;

pub fn router(srv: Arc<Srv>) -> Router {
    let clean = middleware::from_fn(clean_path_redirect);
    Router::new()
        .route("/live", get(live))
        .route("/api/ws", get(websocket))
        .route(
            "/api/v1/queue/upload",
            post(api_queue_upload).layer(middleware::from_fn_with_state(
                srv.clone(),
                api_key_middleware,
            )),
        )
        .route(
            "/api/auth/registration-status",
            get(auth::registration_status),
        )
        .route("/api/auth/login", post(auth::login))
        .route("/api/auth/register", post(auth::register))
        .route("/api/auth/user", get(auth::user))
        .route("/api/auth/logout", post(auth::logout))
        .route("/api/status", get(get_status))
        .route("/api/config", get(get_config).post(save_config))
        .route("/api/config/pending/status", get(pending_status))
        .route("/api/config/pending", get(pending_config))
        .route("/api/config/pending/apply", post(apply_pending))
        .route("/api/config/pending/discard", post(discard_pending))
        .route("/api/queue", get(get_queue).delete(clear_queue))
        .route("/api/queue/add-files", post(add_files))
        .route(
            "/api/queue/stats",
            get(queue_stats).delete(remove_from_queue),
        )
        .route("/api/queue/{id}", delete(remove_from_queue))
        .route("/api/queue/{id}/retry", post(retry_job))
        .route("/api/queue/{id}/cancel", delete(cancel_job))
        .route("/api/queue/{id}/priority", post(set_priority))
        .route("/api/logs", get(get_logs))
        .route("/api/logs/download", get(download_logs))
        .route(
            "/api/upload",
            post(upload).layer(DefaultBodyLimit::disable()),
        )
        .route(
            "/api/upload-folder",
            post(upload_folder).layer(DefaultBodyLimit::disable()),
        )
        .route("/api/upload/cancel", post(cancel_upload))
        .route("/api/nzb/{id}/download", get(download_nzb))
        .route("/api/processor/status", get(processor_status))
        .route("/api/processor/pause", post(pause))
        .route("/api/processor/resume", post(resume))
        .route("/api/processor/paused", get(is_paused))
        .route("/api/processor/auto-paused", get(is_auto_paused))
        .route("/api/processor/auto-pause-reason", get(auto_pause_reason))
        .route("/api/running-jobs", get(running_jobs))
        .route("/api/running-job-details", get(running_job_details))
        .route("/api/validate-server", post(validate_server))
        .route("/api/setup/complete", post(setup_complete))
        .route("/api/metrics/nntp-pool", get(nntp_pool_metrics))
        .route(
            "/api/metrics/transfer-runtime",
            get(transfer_runtime_metrics),
        )
        .route("/api/filesystem/browse", get(browse))
        .route("/api/filesystem/import", post(import_files))
        .route("/api/watcher/status", get(watcher_status))
        .route("/api/watcher/scan", post(trigger_scan))
        .route("/api/arr/instances", get(list_arr).post(add_arr))
        .route("/api/arr/instances/{id}", delete(remove_arr))
        .route("/api/arr/instances/{id}/test", post(test_arr))
        .route("/api/arr/webhook/{instance_id}", post(arr_webhook))
        .route("/api/api-key", get(get_api_key))
        .route("/api/api-key/regenerate", post(regenerate_api_key))
        .fallback(static_handler)
        .method_not_allowed_fallback(static_after_mismatch)
        .layer(middleware::from_fn_with_state(
            srv.clone(),
            auth::middleware,
        ))
        .layer(clean)
        .with_state(srv)
}

/// gorilla/mux's `cleanPath`: a request whose path is not clean (`//`, `.`, `..`) is redirected to the
/// cleaned path before routing.
async fn clean_path_redirect(req: Request, next: middleware::Next) -> Response {
    let path = req.uri().path();
    let mut cleaned = postie_core::backend::app::api::clean_path(&if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    });
    if path.ends_with('/') && cleaned != "/" {
        cleaned.push('/');
    }
    if cleaned != path {
        let target = match req.uri().query() {
            Some(q) => format!("{cleaned}?{q}"),
            None => cleaned,
        };
        let mut r = status_only(StatusCode::MOVED_PERMANENTLY);
        if let Ok(v) = HeaderValue::from_str(&target) {
            r.headers_mut().insert(header::LOCATION, v);
        }
        return r;
    }
    next.run(req).await
}

/// gorilla/mux remembers a method mismatch but keeps matching: the catch-all static route then answers
/// the request, so a wrong method on a known path ends in the static handler too.
async fn static_after_mismatch(State(srv): S, method: Method, uri: axum::http::Uri) -> Response {
    static_files::handle(&srv.build_dir, &method, &uri)
}

async fn static_handler(State(srv): S, method: Method, uri: axum::http::Uri) -> Response {
    static_files::handle(&srv.build_dir, &method, &uri)
}

/// `WriteHeader` before the header is set, so the sniffed type is text/plain.
async fn live() -> Response {
    let mut r = Response::new(axum::body::Body::from("{\"status\":\"live\"}\n"));
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    r
}

async fn websocket(
    State(srv): S,
    session: Option<axum::Extension<auth::Session>>,
    upgrade: WebSocketUpgrade,
) -> Response {
    serve_socket(srv, upgrade, session.map(|e| e.0))
}

fn serve_socket(
    srv: Arc<Srv>,
    upgrade: WebSocketUpgrade,
    session: Option<auth::Session>,
) -> Response {
    use postie_core::wshub::{PING_INTERVAL, WRITE_WAIT};
    upgrade.on_upgrade(move |mut socket| async move {
        let (id, expires) = session.as_ref().map_or((String::new(), None), |s| (s.0.jti.clone(), Some(s.0.exp)));
        let mut sub = srv.hub.register(&id);
        let closed = sub.closed.clone();
        let expiry = expires.map(|e| std::time::Duration::from_secs(u64::try_from(e - chrono_now()).unwrap_or(0)));
        let mut ping = tokio::time::interval(PING_INTERVAL);
        ping.tick().await;
        loop {
            tokio::select! {
                () = closed.cancelled() => break,
                () = async { match expiry { Some(d) => tokio::time::sleep(d).await, None => std::future::pending().await } } => break,
                msg = sub.rx.recv() => {
                    let Some(m) = msg else { break };
                    if tokio::time::timeout(WRITE_WAIT, socket.send(ws::Message::Text(m.to_string().into()))).await.is_err() {
                        break;
                    }
                }
                _ = ping.tick() => {
                    if tokio::time::timeout(WRITE_WAIT, socket.send(ws::Message::Ping(Bytes::new()))).await.map_or(true, |r| r.is_err()) {
                        break;
                    }
                }
                incoming = socket.recv() => {
                    match incoming {
                        Some(Ok(ws::Message::Close(_))) | None | Some(Err(_)) => break,
                        _ => {}
                    }
                }
            }
        }
        srv.hub.unregister(sub.id);
    })
}

fn chrono_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0))
}

// ---- status and config ----

async fn get_status(State(srv): S) -> Response {
    ok_json(&srv.app.get_app_status())
}

async fn get_config(State(srv): S) -> Response {
    match srv.app.get_config() {
        Ok(c) => ok_json(&c),
        Err(e) => internal(&e),
    }
}

async fn save_config(State(srv): S, body: Bytes) -> Response {
    let cfg = match ConfigData::from_json(&String::from_utf8_lossy(&body)) {
        Ok(c) => c,
        Err(e) => return bad_request(&e),
    };
    if let Err(e) = cfg.validate() {
        return bad_request(&e);
    }
    match srv.app.save_config(cfg).await {
        Ok(()) => ok_empty(),
        Err(e) => internal(&e),
    }
}

async fn pending_status(State(srv): S) -> Response {
    ok_json(&srv.app.has_pending_config_changes())
}

async fn pending_config(State(srv): S) -> Response {
    ok_json(&srv.app.pending_config_status())
}

async fn apply_pending(State(srv): S) -> Response {
    let app = srv.app.clone();
    match tokio::task::spawn_blocking(move || app.apply_pending_config()).await {
        Ok(Ok(())) => ok_empty(),
        Ok(Err(e)) => internal(&e),
        Err(e) => internal(&e.to_string()),
    }
}

async fn discard_pending(State(srv): S) -> Response {
    match srv.app.discard_pending_config() {
        Ok(()) => ok_empty(),
        Err(e) => internal(&e),
    }
}

// ---- queue ----

fn query_value(q: &[(String, String)], key: &str) -> String {
    q.iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.clone())
        .unwrap_or_default()
}

async fn get_queue(State(srv): S, Query(q): Query<Vec<(String, String)>>) -> Response {
    let atoi = |k: &str| query_value(&q, k).parse::<i64>().unwrap_or(0);
    let page = atoi("page").max(1);
    let mut limit = atoi("limit");
    if !(1..=100).contains(&limit) {
        limit = 25;
    }
    let mut sort_by = query_value(&q, "sortBy");
    if sort_by.is_empty() {
        sort_by = "created".into();
    }
    let mut order = query_value(&q, "order");
    if order != "asc" && order != "desc" {
        order = "desc".into();
    }
    let mut status = query_value(&q, "status");
    if !matches!(
        status.as_str(),
        "" | "pending" | "complete" | "error" | "running"
    ) {
        status.clear();
    }
    let app = srv.app.clone();
    match tokio::task::spawn_blocking(move || {
        app.get_queue_items(PaginationParams {
            page,
            limit,
            sort_by,
            order,
            status,
        })
    })
    .await
    {
        Ok(Ok(r)) => ok_json(&r),
        Ok(Err(e)) => internal(&e),
        Err(e) => internal(&e.to_string()),
    }
}

async fn clear_queue(State(srv): S) -> Response {
    blocking_unit(srv, |a| a.clear_queue()).await
}

async fn add_files() -> Response {
    // desktop-only dialog in Go; with no dialog the web path reports the same failure shape
    internal("queue not initialized - please restart the application")
}

async fn queue_stats(State(srv): S) -> Response {
    let app = srv.app.clone();
    match tokio::task::spawn_blocking(move || app.get_queue_stats()).await {
        Ok(Ok(s)) => ok_json(&s),
        Ok(Err(e)) => internal(&e),
        Err(e) => internal(&e.to_string()),
    }
}

async fn blocking_unit(
    srv: Arc<Srv>,
    f: impl FnOnce(&postie_core::backend::app::App) -> Result<(), String> + Send + 'static,
) -> Response {
    let app = srv.app.clone();
    match tokio::task::spawn_blocking(move || f(&app)).await {
        Ok(Ok(())) => ok_empty(),
        Ok(Err(e)) => internal(&e),
        Err(e) => internal(&e.to_string()),
    }
}

async fn remove_from_queue(State(srv): S, Path(id): Path<String>) -> Response {
    blocking_unit(srv, move |a| a.remove_from_queue(&id)).await
}

async fn retry_job(State(srv): S, Path(id): Path<String>) -> Response {
    blocking_unit(srv, move |a| a.retry_job(&id)).await
}

async fn cancel_job(State(srv): S, Path(id): Path<String>) -> Response {
    blocking_unit(srv, move |a| a.cancel_job(&id)).await
}

#[derive(Deserialize)]
struct PriorityBody {
    #[serde(default)]
    priority: i64,
}

async fn set_priority(State(srv): S, Path(id): Path<String>, body: Bytes) -> Response {
    let b: PriorityBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(e) => return bad_request(&e.to_string()),
    };
    blocking_unit(srv, move |a| a.set_queue_item_priority(&id, b.priority)).await
}

async fn cancel_upload() -> Response {
    internal("use CancelJob() to cancel individual jobs instead")
}

async fn download_nzb(State(srv): S, Path(id): Path<String>) -> Response {
    let app = srv.app.clone();
    match tokio::task::spawn_blocking(move || app.get_nzb(&id)).await {
        Ok(Ok((data, name))) => {
            let mut r = Response::new(axum::body::Body::from(data.clone()));
            let h = r.headers_mut();
            h.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/x-nzb"),
            );
            if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
                h.insert(header::CONTENT_DISPOSITION, v);
            }
            h.insert(header::CONTENT_LENGTH, HeaderValue::from(data.len()));
            r
        }
        Ok(Err(e)) => internal(&e),
        Err(e) => internal(&e.to_string()),
    }
}

// ---- logs ----

async fn get_logs(State(srv): S, Query(q): Query<Vec<(String, String)>>) -> Response {
    let limit = query_value(&q, "limit").parse::<i64>().unwrap_or(100);
    let offset = query_value(&q, "offset").parse::<i64>().unwrap_or(0);
    let limit = if query_value(&q, "limit").is_empty() {
        100
    } else {
        limit
    };
    match srv.app.get_logs_paginated(limit, offset) {
        Ok(logs) => {
            let mut r = Response::new(axum::body::Body::from(logs));
            r.headers_mut()
                .insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain"));
            r
        }
        Err(e) => internal(&e),
    }
}

async fn download_logs(State(srv): S) -> Response {
    match srv.app.get_logs() {
        Ok(logs) => {
            let name = format!("postie-{}.log", chrono_date());
            let mut r = Response::new(axum::body::Body::from(logs.clone()));
            let h = r.headers_mut();
            h.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; charset=utf-8"),
            );
            if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}\"")) {
                h.insert(header::CONTENT_DISPOSITION, v);
            }
            h.insert(header::CONTENT_LENGTH, HeaderValue::from(logs.len()));
            r
        }
        Err(e) => internal(&e),
    }
}

fn chrono_date() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

// ---- uploads ----

fn temp_dir(prefix: &str) -> std::io::Result<std::path::PathBuf> {
    let p = std::env::temp_dir().join(format!("{prefix}{}", rand::random::<u32>()));
    std::fs::create_dir_all(&p)?;
    Ok(p)
}

async fn upload(State(srv): S, mut mp: Multipart) -> Response {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    loop {
        match mp.next_field().await {
            Ok(Some(f)) => {
                if f.name() == Some("files") {
                    let name = f.file_name().unwrap_or("").to_string();
                    match f.bytes().await {
                        Ok(b) => files.push((name, b.to_vec())),
                        Err(e) => return bad_request(&e.to_string()),
                    }
                }
            }
            Ok(None) => break,
            Err(e) => return bad_request(&e.to_string()),
        }
    }
    if files.is_empty() {
        return bad_request("No files provided");
    }
    srv.hub.emit_event(
        "upload-progress",
        json!({"stage": "saving", "progress": 0, "fileCount": files.len()}),
    );
    let Ok(dir) = temp_dir("postie-") else {
        return internal("Failed to create temporary directory");
    };
    let mut paths = Vec::new();
    let count = files.len();
    for (i, (name, data)) in files.into_iter().enumerate() {
        let base = std::path::Path::new(&name)
            .file_name()
            .map(std::ffi::OsStr::to_owned)
            .unwrap_or_default();
        let p = dir.join(base);
        if std::fs::write(&p, data).is_err() {
            return internal("Failed to create temporary file");
        }
        paths.push(p.to_string_lossy().into_owned());
        srv.hub.emit_event("upload-progress", json!({"stage": "saving", "progress": (i + 1) as f64 / count as f64 * 50.0, "currentFile": name, "fileCount": count}));
    }
    srv.hub.emit_event(
        "upload-progress",
        json!({"stage": "processing", "progress": 75, "fileCount": count}),
    );
    if let Err(e) = srv.app.handle_dropped_files(&paths) {
        srv.hub.emit_event("upload-error", e.clone());
        return internal(&e);
    }
    srv.hub.emit_event(
        "upload-complete",
        json!({"fileCount": count, "message": "Files successfully processed and added to queue"}),
    );
    ok_empty()
}

async fn upload_folder(State(srv): S, mut mp: Multipart) -> Response {
    let mut folder_name = String::new();
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    let mut paths: Vec<String> = Vec::new();
    loop {
        match mp.next_field().await {
            Ok(Some(f)) => {
                let name = f.name().unwrap_or("").to_string();
                match name.as_str() {
                    "folderName" => folder_name = f.text().await.unwrap_or_default(),
                    "paths" => paths.push(f.text().await.unwrap_or_default()),
                    "files" => {
                        let fname = f.file_name().unwrap_or("").to_string();
                        match f.bytes().await {
                            Ok(b) => files.push((fname, b.to_vec())),
                            Err(e) => return bad_request(&e.to_string()),
                        }
                    }
                    _ => {}
                }
            }
            Ok(None) => break,
            Err(e) => return bad_request(&e.to_string()),
        }
    }
    if folder_name.is_empty() {
        return bad_request("Folder name is required");
    }
    if files.is_empty() {
        return bad_request("No files provided");
    }
    if files.len() != paths.len() {
        return bad_request("Mismatch between files and paths count");
    }
    let count = files.len();
    srv.hub.emit_event(
        "upload-progress",
        json!({"stage": "saving", "progress": 0, "fileCount": count, "folderName": folder_name}),
    );
    let Ok(dir) = temp_dir("postie-folder-") else {
        return internal("Failed to create temporary directory");
    };
    for (i, ((fname, data), rel)) in files.into_iter().zip(&paths).enumerate() {
        let dest = dir.join(rel.trim_start_matches('/'));
        if dest
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return internal("Failed to create directory structure");
        }
        if let Some(parent) = dest.parent()
            && std::fs::create_dir_all(parent).is_err()
        {
            return internal("Failed to create directory structure");
        }
        if std::fs::write(&dest, data).is_err() {
            return internal("Failed to create file");
        }
        srv.hub.emit_event("upload-progress", json!({"stage": "saving", "progress": (i + 1) as f64 / count as f64 * 50.0, "currentFile": fname, "fileCount": count, "folderName": folder_name}));
    }
    srv.hub.emit_event("upload-progress", json!({"stage": "processing", "progress": 75, "fileCount": count, "folderName": folder_name}));
    let folder_path = dir.join(&folder_name).to_string_lossy().into_owned();
    if let Err(e) = srv.app.handle_dropped_files(&[folder_path]) {
        srv.hub.emit_event("upload-error", e.clone());
        return internal(&e);
    }
    srv.hub.emit_event("upload-complete", json!({"fileCount": count, "folderName": folder_name, "message": "Folder successfully processed and added to queue as single NZB"}));
    ok_empty()
}

// ---- processor ----

async fn processor_status(State(srv): S) -> Response {
    ok_json(&srv.app.get_processor_status())
}

async fn pause(State(srv): S) -> Response {
    match srv.app.pause_processing() {
        Ok(()) => ok_empty(),
        Err(e) => internal(&e),
    }
}

async fn resume(State(srv): S) -> Response {
    match srv.app.resume_processing() {
        Ok(()) => ok_empty(),
        Err(e) => internal(&e),
    }
}

async fn is_paused(State(srv): S) -> Response {
    ok_json(&json!({"paused": srv.app.is_processing_paused()}))
}

async fn is_auto_paused(State(srv): S) -> Response {
    ok_json(&json!({"autoPaused": srv.app.is_processing_auto_paused()}))
}

async fn auto_pause_reason(State(srv): S) -> Response {
    ok_json(&json!({"reason": srv.app.auto_pause_reason()}))
}

async fn running_jobs(State(srv): S) -> Response {
    ok_json(&srv.app.get_running_jobs())
}

async fn running_job_details(State(srv): S) -> Response {
    ok_json(&srv.app.get_running_jobs_details())
}

// ---- servers, setup, metrics ----

async fn validate_server(State(srv): S, body: Bytes) -> Response {
    let data: ServerData = match serde_json::from_slice(&body) {
        Ok(d) => d,
        Err(e) => return bad_request(&e.to_string()),
    };
    ok_json(&srv.app.validate_nntp_server(data).await)
}

fn setup_error(status: StatusCode, code: &str, message: &str, details: &str) -> Response {
    let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let mut obj = json!({"error": code, "message": message, "code": code, "timestamp": ts});
    if !details.is_empty() {
        obj["details"] = json!(details);
    }
    // field order of Go's ErrorResponse: error, message, code, details, timestamp
    let ordered = format!(
        "{{\"error\":{},\"message\":{},\"code\":{}{},\"timestamp\":{}}}\n",
        json!(code),
        json!(message),
        json!(code),
        if details.is_empty() {
            String::new()
        } else {
            format!(",\"details\":{}", json!(details))
        },
        json!(ts)
    );
    let mut r = Response::new(axum::body::Body::from(ordered));
    *r.status_mut() = status;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    let _ = obj;
    r
}

fn categorize_setup_error(err: &str) -> (&'static str, &'static str, &'static str) {
    if err.contains("failed to connect") || err.contains("connection test failed") {
        return (
            "SERVER_VALIDATION_FAILED",
            "Server connection failed",
            "Please check your server credentials and network connectivity. Ensure the server is reachable and your credentials are correct.",
        );
    }
    if err.contains("permission denied") || err.contains("read-only file system") {
        return (
            "FILESYSTEM_ERROR",
            "File system permission error",
            "Unable to save configuration file. Please check file permissions and ensure the application has write access to the configuration directory.",
        );
    }
    if err.contains("failed to save") || err.contains("error writing config") {
        return (
            "CONFIG_SAVE_FAILED",
            "Configuration save failed",
            "Failed to save configuration file. Please ensure sufficient disk space and write permissions.",
        );
    }
    if err.contains("server") && (err.contains("host is required") || err.contains("port must be"))
    {
        return (
            "INVALID_INPUT",
            "Invalid server configuration",
            "Please verify all server fields are properly filled out with valid values.",
        );
    }
    if err.contains("output directory") {
        return (
            "INVALID_INPUT",
            "Invalid output directory",
            "Please specify a valid output directory path.",
        );
    }
    (
        "INTERNAL_ERROR",
        "Setup failed",
        "An unexpected error occurred during setup. Please try again or contact support if the problem persists.",
    )
}

async fn setup_complete(State(srv): S, body: Bytes) -> Response {
    let data: SetupWizardData = match serde_json::from_slice(&body) {
        Ok(d) => d,
        Err(_) => {
            return setup_error(
                StatusCode::BAD_REQUEST,
                "INVALID_INPUT",
                "Invalid request data",
                "The request body contains invalid JSON. Please ensure all required fields are properly formatted.",
            );
        }
    };
    match srv.app.setup_wizard_complete(data).await {
        Ok(()) => {
            let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
            ok_json(
                &json!({"message": "Setup completed successfully", "success": true, "timestamp": ts}),
            )
        }
        Err(e) => {
            let (code, msg, details) = categorize_setup_error(&e);
            let status = if code == "INVALID_INPUT" {
                StatusCode::BAD_REQUEST
            } else {
                StatusCode::INTERNAL_SERVER_ERROR
            };
            setup_error(status, code, msg, details)
        }
    }
}

async fn nntp_pool_metrics(State(srv): S) -> Response {
    match srv.app.get_nntp_pool_metrics() {
        Ok(m) => ok_json(&m),
        Err(e) => internal(&e),
    }
}

async fn transfer_runtime_metrics(State(srv): S) -> Response {
    ok_json(&srv.app.get_transfer_runtime_metrics())
}

// ---- filesystem ----

async fn browse(Query(q): Query<Vec<(String, String)>>) -> Response {
    let mut target = query_value(&q, "path");
    if target.is_empty() {
        target = "/".into();
    }
    let mut target = postie_core::backend::app::api::clean_path(&target);
    if !target.starts_with('/') {
        target = format!("/{target}");
    }
    let path = target.clone();
    match tokio::task::spawn_blocking(move || browse_directory(&path)).await {
        Ok(Ok(items)) => ok_json(&json!({"path": target, "items": items})),
        Ok(Err(e)) => internal(&format!("Failed to browse directory: {e}")),
        Err(e) => internal(&e.to_string()),
    }
}

fn browse_directory(dir: &str) -> Result<Value, String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("open {dir}: {}", io_text(&e)))?;
    let mut entries: Vec<_> = rd.flatten().collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let mut items = Vec::new();
    for e in entries {
        let Ok(meta) = std::fs::symlink_metadata(e.path()) else {
            continue;
        };
        let name = e.file_name().to_string_lossy().into_owned();
        let mod_time = meta
            .modified()
            .ok()
            .map(|m| {
                chrono::DateTime::<chrono::Local>::from(m)
                    .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            })
            .unwrap_or_default();
        items.push(json!({
            "name": name,
            "path": std::path::Path::new(dir).join(&name).to_string_lossy(),
            "isDir": meta.is_dir(),
            "size": meta.len(),
            "modTime": mod_time,
        }));
    }
    Ok(if items.is_empty() {
        Value::Null
    } else {
        Value::Array(items)
    })
}

/// Go's `syscall.Errno` text (lower-cased `strerror`).
fn io_text(e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => "no such file or directory".into(),
        std::io::ErrorKind::PermissionDenied => "permission denied".into(),
        _ => e.to_string().to_lowercase(),
    }
}

#[derive(Deserialize)]
struct ImportBody {
    #[serde(default, rename = "filePaths")]
    file_paths: Vec<String>,
}

async fn import_files(State(srv): S, body: Bytes) -> Response {
    let b: ImportBody = match serde_json::from_slice(&body) {
        Ok(b) => b,
        Err(_) => return bad_request("Invalid request body"),
    };
    if b.file_paths.is_empty() {
        return bad_request("No file paths provided");
    }
    let count = b.file_paths.len();
    srv.hub.emit_event(
        "import-progress",
        json!({"stage": "validating", "progress": 0, "fileCount": count}),
    );
    let mut valid = Vec::new();
    for (i, p) in b.file_paths.iter().enumerate() {
        let clean = postie_core::backend::app::api::clean_path(p);
        if !std::path::Path::new(&clean).is_absolute() || std::fs::metadata(&clean).is_err() {
            continue;
        }
        valid.push(clean);
        srv.hub.emit_event("import-progress", json!({"stage": "validating", "progress": (i + 1) as f64 / count as f64 * 50.0, "currentFile": std::path::Path::new(p).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), "fileCount": count}));
    }
    if valid.is_empty() {
        srv.hub
            .emit_event("import-error", "No valid files found to import");
        return bad_request("No valid files found to import");
    }
    srv.hub.emit_event(
        "import-progress",
        json!({"stage": "processing", "progress": 75, "fileCount": valid.len()}),
    );
    if let Err(e) = srv.app.handle_dropped_files(&valid) {
        srv.hub.emit_event(
            "import-error",
            format!("Failed to process imported files: {e}"),
        );
        return internal(&e);
    }
    srv.hub.emit_event("import-complete", json!({"fileCount": valid.len(), "message": format!("Successfully imported {} files from remote filesystem", valid.len())}));
    ok_json(
        &json!({"importedCount": valid.len(), "message": format!("Successfully imported {} files", valid.len()), "success": true}),
    )
}

// ---- watchers ----

async fn watcher_status(State(srv): S) -> Response {
    ok_json(&srv.app.get_watcher_status())
}

async fn trigger_scan(State(srv): S) -> Response {
    srv.app.trigger_scan();
    ok_json(&json!({"status": "ok"}))
}

// ---- API key and gated API ----

async fn get_api_key(State(srv): S) -> Response {
    match srv.app.get_api_key() {
        Ok(k) => ok_json(&json!({"key": k})),
        Err(e) => internal(&e),
    }
}

async fn regenerate_api_key(State(srv): S) -> Response {
    match srv.app.regenerate_api_key() {
        Ok(k) => ok_json(&json!({"key": k})),
        Err(e) => internal(&e),
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

async fn api_key_middleware(State(srv): S, req: Request, next: middleware::Next) -> Response {
    if !srv.app.is_api_enabled() {
        return text_error(StatusCode::FORBIDDEN, "api disabled");
    }
    let expected = match srv.app.get_api_key() {
        Ok(k) if !k.is_empty() => k,
        _ => return internal("api key not available"),
    };
    let h = req.headers();
    let mut got = h
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    if got.is_empty()
        && let Some(b) = h
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
    {
        got = b.to_string();
    }
    if got.is_empty() {
        return text_error(StatusCode::UNAUTHORIZED, "missing api key");
    }
    if !constant_time_eq(got.as_bytes(), expected.as_bytes()) {
        return text_error(StatusCode::UNAUTHORIZED, "invalid api key");
    }
    next.run(req).await
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApiUploadRequest {
    file: String,
    relative_path: String,
    priority: i64,
    delete_after_upload: bool,
}

async fn api_queue_upload(State(srv): S, body: Bytes) -> Response {
    let req: ApiUploadRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return bad_request(&format!("invalid request body: {e}")),
    };
    match srv.app.enqueue_api_upload(
        &req.file,
        &req.relative_path,
        req.priority,
        req.delete_after_upload,
    ) {
        Ok(v) => json_response(StatusCode::ACCEPTED, &v),
        Err(msg) => {
            if msg.contains("file not found") {
                return text_error(StatusCode::NOT_FOUND, &msg);
            }
            if [
                "file is required",
                "relative_path is required",
                "file must be",
                "relative_path must be",
                "relative_path ",
            ]
            .iter()
            .any(|p| msg.starts_with(p))
            {
                return bad_request(&msg);
            }
            if msg.contains("api key store not initialized")
                || msg == "queue not initialized" && false
            {
                return text_error(StatusCode::SERVICE_UNAVAILABLE, &msg);
            }
            internal(&msg)
        }
    }
}

// ---- arr ----

async fn list_arr(State(srv): S) -> Response {
    match srv.app.get_arr_instances() {
        Ok(i) => ok_json(&i),
        Err(e) => internal(&e),
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AddArrRequest {
    instance: postie_core::config::ArrInstance,
    webhook_url: String,
}

async fn add_arr(State(srv): S, body: Bytes) -> Response {
    let mut req: AddArrRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => return bad_request(&e.to_string()),
    };
    if req.instance.id.is_empty() {
        req.instance.id = uuid::Uuid::new_v4().to_string();
    }
    let key = match srv.app.get_api_key() {
        Ok(k) => k,
        Err(e) => return internal(&format!("could not retrieve API key: {e}")),
    };
    let url = format!(
        "{}/api/arr/webhook/{}?apiKey={}",
        req.webhook_url.trim_end_matches('/'),
        req.instance.id,
        key
    );
    match srv.app.setup_arr_webhook(req.instance, &url).await {
        Ok(saved) => json_response(StatusCode::CREATED, &saved),
        Err(e) => internal(&e),
    }
}

async fn remove_arr(State(srv): S, Path(id): Path<String>) -> Response {
    match srv.app.remove_arr_instance(&id).await {
        Ok(()) => status_only(StatusCode::NO_CONTENT),
        Err(e) => internal(&e),
    }
}

async fn test_arr(State(srv): S, body: Bytes) -> Response {
    let inst: postie_core::config::ArrInstance = match serde_json::from_slice(&body) {
        Ok(i) => i,
        Err(e) => return bad_request(&e.to_string()),
    };
    match srv.app.test_arr_connection(&inst).await {
        Ok(()) => ok_empty(),
        Err(e) => text_error(StatusCode::BAD_GATEWAY, &e),
    }
}

async fn arr_webhook(
    State(srv): S,
    Path(instance_id): Path<String>,
    Query(q): Query<Vec<(String, String)>>,
    body: Bytes,
) -> Response {
    let Ok(key) = srv.app.get_api_key() else {
        return internal("internal error");
    };
    if !constant_time_eq(query_value(&q, "apiKey").as_bytes(), key.as_bytes()) {
        return text_error(StatusCode::UNAUTHORIZED, "unauthorized");
    }
    let payload: postie_core::arr::WebhookPayload = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => return bad_request(&e.to_string()),
    };
    let paths = postie_core::arr::extract_file_paths(&payload);
    if paths.is_empty() {
        return ok_empty();
    }
    let instances = match srv.app.get_arr_instances() {
        Ok(i) => i,
        Err(e) => return internal(&e),
    };
    let Some(inst) = instances.iter().find(|i| i.id == instance_id) else {
        return text_error(StatusCode::NOT_FOUND, "arr instance not found");
    };
    for p in paths {
        let dir = std::path::Path::new(&p)
            .parent()
            .map(|d| d.to_string_lossy().into_owned())
            .unwrap_or_else(|| ".".into());
        if let Err(e) = srv
            .app
            .enqueue_api_upload(&p, &dir, 0, inst.delete_after_upload)
        {
            return internal(&format!("queuing {p}: {e}"));
        }
    }
    ok_empty()
}
