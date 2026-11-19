//! Response helpers that reproduce Go's `net/http` details byte for byte.

use axum::body::Body;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use serde::Serialize;

/// `http.Error`: plain text plus a trailing newline, with `nosniff`.
pub fn text_error(status: StatusCode, msg: &str) -> Response {
    let mut r = Response::new(Body::from(format!("{msg}\n")));
    *r.status_mut() = status;
    let h = r.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    h.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    r
}

pub fn not_found() -> Response {
    text_error(StatusCode::NOT_FOUND, "404 page not found")
}

/// `json.NewEncoder(w).Encode(v)`: compact JSON and a newline.
pub fn json_response<T: Serialize>(status: StatusCode, v: &T) -> Response {
    let mut body = serde_json::to_vec(v).unwrap_or_else(|_| b"null".to_vec());
    body.push(b'\n');
    let mut r = Response::new(Body::from(body));
    *r.status_mut() = status;
    r.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    r
}

pub fn ok_json<T: Serialize>(v: &T) -> Response {
    json_response(StatusCode::OK, v)
}

/// `w.WriteHeader(status)` with no body.
pub fn status_only(status: StatusCode) -> Response {
    let mut r = Response::new(Body::empty());
    *r.status_mut() = status;
    r
}

pub fn ok_empty() -> Response {
    status_only(StatusCode::OK)
}

pub fn internal(msg: &str) -> Response {
    text_error(StatusCode::INTERNAL_SERVER_ERROR, msg)
}

pub fn bad_request(msg: &str) -> Response {
    text_error(StatusCode::BAD_REQUEST, msg)
}

/// The CORS headers an unauthenticated deployment sends on everything.
pub fn add_cors(r: &mut Response) {
    let h = r.headers_mut();
    h.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    h.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("GET, POST, PUT, DELETE, OPTIONS"),
    );
    h.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("Content-Type, Authorization"),
    );
}
