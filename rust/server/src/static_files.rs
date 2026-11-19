//! The SPA static handler of `getStaticFileHandler`, with `http.FileServer`'s observable behaviour:
//! an existing file is served; a path without extension tries `<path>.html`, then falls back to
//! `/index.html`, which `FileServer` answers with a 301 to `./`; a missing asset is a 404.

use std::path::{Component, Path, PathBuf};

use axum::body::Body;
use axum::http::{HeaderValue, Method, StatusCode, Uri, header};
use axum::response::Response;

use crate::util::{not_found, status_only};

fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "txt" => "text/plain; charset=utf-8",
        "xml" => "text/xml; charset=utf-8",
        "map" => "application/json",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

/// Resolves a request path inside `root` like `http.Dir` (cleaned, never escaping the root).
fn resolve(root: &Path, request_path: &str) -> Option<PathBuf> {
    let mut out = root.to_path_buf();
    for c in Path::new(request_path.trim_start_matches('/')).components() {
        match c {
            Component::Normal(s) => out.push(s),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

fn serve_file(path: &Path, method: &Method) -> Response {
    let Ok(data) = std::fs::read(path) else {
        return not_found();
    };
    let mut r = Response::new(if method == Method::HEAD {
        Body::empty()
    } else {
        Body::from(data.clone())
    });
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(content_type(path)),
    );
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(data.len()));
    r
}

pub fn handle(root: &Path, method: &Method, uri: &Uri) -> Response {
    // `r.URL.Path` is the decoded path
    let path = percent_decode(uri.path());
    let is_file = |p: &str| resolve(root, p).is_some_and(|f| f.is_file());
    if is_file(&path) {
        // FileServer redirects a request for an index file to its directory
        if path.ends_with("/index.html") {
            return redirect_to_dir(&path);
        }
        return resolve(root, &path).map_or_else(not_found, |f| serve_file(&f, method));
    }
    let has_ext = Path::new(&path).extension().is_some();
    if !has_ext {
        let html = format!("{path}.html");
        if is_file(&html) {
            return resolve(root, &html).map_or_else(not_found, |f| serve_file(&f, method));
        }
        // SPA fallback: FileServer answers /index.html with a redirect to "./"
        return redirect_to_dir("/index.html");
    }
    not_found()
}

fn redirect_to_dir(path: &str) -> Response {
    let _ = path;
    let mut r = status_only(StatusCode::MOVED_PERMANENTLY);
    r.headers_mut()
        .insert(header::LOCATION, HeaderValue::from_static("./"));
    r
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h * 16 + l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(c: u8) -> Option<u8> {
    (c as char).to_digit(16).map(|d| d as u8)
}
