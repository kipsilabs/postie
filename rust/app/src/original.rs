use std::ffi::OsString;
use std::io::Write;
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
const RUN: &[&str] = &["env", "PORT={port}", "rust/original/run.sh"];
const FALLBACK_LOG: &str = "RUST_ANYTHING_FALLBACK_LOG";

fn resolve(word: &str, port: u16) -> OsString {
    let word = word.replace("{port}", &port.to_string());
    if word.starts_with('-') {
        return word.into();
    }
    let path = Path::new(ROOT).join(&word);
    let looks_like_path = word.contains('/') || word.contains('\\') || path.is_file();
    if looks_like_path && path.exists() {
        return std::path::absolute(&path).unwrap_or(path).into_os_string();
    }
    word.into()
}

fn log() -> Option<std::fs::File> {
    let path = std::env::var_os(FALLBACK_LOG)?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

/// Tells `rust-anything app replay` this build counts its fallbacks, even when it makes none.
pub fn report_fallbacks() {
    let _ = log();
}

/// The original server, started on a free port of its own.
#[derive(Clone)]
pub struct Original {
    base: String,
    client: reqwest::Client,
    child: Arc<Mutex<Option<Child>>>,
}

impl Original {
    pub async fn start() -> Self {
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map(|a| a.port())
            .expect("a free port");
        let (program, rest) = RUN.split_first().expect("the original's command");
        let mut child = Command::new(resolve(program, port))
            .args(rest.iter().map(|w| resolve(w, port)))
            .current_dir(ROOT)
            .env("PORT", port.to_string())
            .env_remove(FALLBACK_LOG)
            .stdin(Stdio::null())
            .spawn()
            .expect("starting the original");
        let deadline = Instant::now() + Duration::from_secs(60);
        while tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err()
        {
            if let Ok(Some(status)) = child.try_wait() {
                panic!("the original exited ({status}) before listening on port {port}");
            }
            assert!(
                Instant::now() < deadline,
                "the original did not listen on port {port}"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("an HTTP client");
        Original {
            base: format!("http://127.0.0.1:{port}"),
            client,
            child: Arc::new(Mutex::new(Some(child))),
        }
    }

    pub fn stop(&self) {
        if let Ok(mut slot) = self.child.lock()
            && let Some(mut child) = slot.take()
        {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Sends the request to the original and passes its answer through. Each call is one fallback.
pub async fn proxy(State(original): State<Original>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let target = parts
        .uri
        .path_and_query()
        .map_or("/", |p| p.as_str())
        .to_string();
    if let Some(mut file) = log() {
        let _ = writeln!(file, "{} {target}", parts.method);
    }
    let Ok(body) = axum::body::to_bytes(body, usize::MAX).await else {
        return (
            StatusCode::BAD_REQUEST,
            "front door: unreadable request body",
        )
            .into_response();
    };
    let mut request = original
        .client
        .request(parts.method, format!("{}{target}", original.base))
        .body(body);
    for (name, value) in &parts.headers {
        if name != header::HOST {
            request = request.header(name, value);
        }
    }
    match request.send().await {
        Ok(answer) => {
            let mut response = Response::builder().status(answer.status());
            for (name, value) in answer.headers() {
                if name != header::TRANSFER_ENCODING && name != header::CONNECTION {
                    response = response.header(name, value);
                }
            }
            let body = answer.bytes().await.unwrap_or_default();
            response
                .body(Body::from(body))
                .unwrap_or_else(|_| StatusCode::BAD_GATEWAY.into_response())
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            format!("front door: the original did not answer: {e}"),
        )
            .into_response(),
    }
}

/// Ctrl-C or SIGTERM: stop taking requests and finish the open ones.
pub async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("a SIGTERM handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}
