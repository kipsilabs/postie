//! Port of `cmd/web/auth.go`: the session middleware, login/register/logout handlers and the login
//! attempt limiter.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use postie_core::webauth::{AuthError, Claims, SESSION_DURATION_SECS};
use serde::Deserialize;
use serde_json::json;

use crate::Srv;
use crate::util::{add_cors, internal, json_response, not_found, ok_json, status_only, text_error};

pub const SESSION_COOKIE: &str = "postie_session";

/// Sessions the middleware attached to the request.
#[derive(Clone)]
pub struct Session(pub Claims);

fn secure_cookies() -> bool {
    std::env::var("POSTIE_COOKIE_SECURE").is_ok_and(|v| v == "true")
}

/// Ignores client-supplied proxy headers deliberately: proxies should preserve Host; HTTPS deployments
/// terminate TLS here or set `POSTIE_COOKIE_SECURE`.
fn same_origin(headers: &HeaderMap) -> bool {
    same_origin_with(headers, false)
}

/// `any_scheme` accepts http or https, for unauthenticated deployments that may sit behind a TLS proxy.
fn same_origin_with(headers: &HeaderMap, any_scheme: bool) -> bool {
    let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) else {
        return true;
    };
    if origin.is_empty() {
        return true;
    }
    let scheme = if secure_cookies() { "https" } else { "http" };
    let rest = if any_scheme {
        origin
            .strip_prefix("http://")
            .or_else(|| origin.strip_prefix("https://"))
    } else {
        origin.strip_prefix(&format!("{scheme}://"))
    };
    let Some(rest) = rest else {
        return false;
    };
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    !rest.is_empty() && rest == host && !rest.contains(['/', '?', '#', '@'])
}

/// With auth off the server only answers requests addressed to loopback, an IP literal or a host
/// listed in `POSTIE_ALLOWED_HOSTS` (comma separated, with or without port). This defeats DNS
/// rebinding, where a hostile page's own hostname resolves to this machine.
fn host_allowed(headers: &HeaderMap) -> bool {
    let Some(host) = headers.get(header::HOST).and_then(|v| v.to_str().ok()) else {
        return false;
    };
    let allowed = std::env::var("POSTIE_ALLOWED_HOSTS").unwrap_or_default();
    host_allowed_with(host, &allowed)
}

const MAX_LOGGED_HOSTS: usize = 32;

/// Records `host` in `seen` and reports whether it is new; the set never exceeds `MAX_LOGGED_HOSTS`.
fn first_rejection(seen: &Mutex<HashSet<String>>, host: &str) -> bool {
    let Ok(mut seen) = seen.lock() else {
        return false;
    };
    seen.len() < MAX_LOGGED_HOSTS && seen.insert(host.to_string())
}

/// Prints one stderr line per distinct rejected Host so a locked-out operator can see why.
fn log_rejected_host(headers: &HeaderMap) {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let host: String = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .chars()
        .filter(|c| !c.is_control())
        .take(255)
        .collect();
    if first_rejection(SEEN.get_or_init(Default::default), &host) {
        eprintln!(
            "postie: rejected request for Host \"{host}\" (authentication is off); add it to POSTIE_ALLOWED_HOSTS or enable authentication"
        );
    }
}

fn host_allowed_with(host: &str, allowed: &str) -> bool {
    let host = host.trim().to_ascii_lowercase();
    let (name, with_port) = match host.strip_prefix('[') {
        Some(rest) => match rest.split_once(']') {
            Some((ip, _)) => (ip.to_string(), host.clone()),
            None => return false,
        },
        None => (
            host.rsplit_once(':')
                .filter(|(h, _)| !h.contains(':'))
                .map_or(host.clone(), |(h, _)| h.to_string()),
            host.clone(),
        ),
    };
    if name == "localhost"
        || name.ends_with(".localhost")
        || name.parse::<std::net::IpAddr>().is_ok()
    {
        return true;
    }
    allowed
        .split(',')
        .map(|a| a.trim().to_ascii_lowercase())
        .filter(|a| !a.is_empty())
        .any(|a| a == name || a == with_port)
}

fn cookie_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|p| p.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.to_string())
}

fn redirect(to: &str) -> Response {
    let mut r = status_only(StatusCode::SEE_OTHER);
    if let Ok(v) = HeaderValue::from_str(to) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    r
}

fn is_static_shell(path: &str) -> bool {
    path == "/live"
        || path == "/login"
        || path == "/login.html"
        || path.starts_with("/_app/")
        || path == "/favicon.ico"
}

pub async fn middleware(State(srv): State<Arc<Srv>>, mut req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let method = req.method().clone();
    if !srv.auth_required {
        if path == "/api/auth/user" && method == Method::GET {
            let mut r = ok_json(&json!({"enabled": false}));
            r.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            return r;
        }
        if path.starts_with("/api/auth/") {
            return not_found();
        }
        if path == "/login" || path == "/login.html" {
            return redirect("/");
        }
        // The key-protected `/api/v1/*` API and the arr webhook enforce their own API key, so they
        // keep the open CORS policy and are bound neither to the Origin check nor to the Host
        // allowlist (a browser preflight there succeeds, so the request itself must not be refused).
        let keyed = path.starts_with("/api/v1/") || path.starts_with("/api/arr/webhook/");
        if !keyed {
            // A cross-origin page can send a state-changing "simple" request (text/plain POST) without
            // a preflight, and the config it rewrites includes the post-upload script. Browsers always
            // send Origin on such requests, so refuse any that is not this server's own origin.
            // Clients that send no Origin (curl, integrations) are unaffected.
            let unsafe_method =
                method != Method::GET && method != Method::HEAD && method != Method::OPTIONS;
            if unsafe_method && !same_origin_with(req.headers(), true) {
                return text_error(StatusCode::FORBIDDEN, "Cross-origin request rejected");
            }
            // DNS rebinding makes a hostile page's Origin and Host match, so also pin the Host.
            if !host_allowed(req.headers()) {
                log_rejected_host(req.headers());
                return text_error(
                    StatusCode::FORBIDDEN,
                    "Host not allowed; add it to POSTIE_ALLOWED_HOSTS or enable authentication",
                );
            }
            // A cross-origin page can open the websocket and read the live queue.
            if path == "/api/ws" && !same_origin_with(req.headers(), true) {
                return text_error(StatusCode::FORBIDDEN, "Cross-origin request rejected");
            }
        }
        if method == Method::OPTIONS {
            let mut r = status_only(StatusCode::OK);
            if path.starts_with("/api/v1/") {
                add_cors(&mut r);
            }
            return r;
        }
        let mut r = next.run(req).await;
        // Wildcard CORS on the config API would let any website read NNTP passwords, the API key and
        // directory listings.
        if path.starts_with("/api/v1/") {
            add_cors(&mut r);
        }
        return r;
    }
    // integrations enforce their own API key, never a browser cookie
    let is_arr_receiver = method == Method::POST
        && path
            .strip_prefix("/api/arr/webhook/")
            .is_some_and(|rest| !rest.contains('/'));
    if path.starts_with("/api/v1/") || is_arr_receiver {
        return next.run(req).await;
    }
    if !same_origin(req.headers()) {
        return text_error(StatusCode::FORBIDDEN, "Cross-origin request rejected");
    }
    let unsafe_method =
        method != Method::GET && method != Method::HEAD && method != Method::OPTIONS;
    let content_json = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if unsafe_method && !req.headers().contains_key(header::ORIGIN) && !content_json {
        return text_error(
            StatusCode::FORBIDDEN,
            "JSON or same-origin request required",
        );
    }
    if is_static_shell(&path) {
        return next.run(req).await;
    }
    let Some(auth) = srv.auth.clone() else {
        return text_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Authentication unavailable",
        );
    };
    if path == "/api/auth/login"
        || path == "/api/auth/register"
        || path == "/api/auth/registration-status"
    {
        return next.run(req).await;
    }
    let claims = cookie_value(req.headers(), SESSION_COOKIE).and_then(|c| auth.validate(&c).ok());
    let Some(claims) = claims else {
        return if path.starts_with("/api/") {
            text_error(StatusCode::UNAUTHORIZED, "Authentication required")
        } else {
            redirect("/login")
        };
    };
    req.extensions_mut().insert(Session(claims));
    let mut r = next.run(req).await;
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn set_session_cookie(r: &mut Response, value: &str, max_age: i64) {
    let secure = if secure_cookies() { "; Secure" } else { "" };
    let c = format!(
        "{SESSION_COOKIE}={value}; Path=/; Max-Age={max_age}; HttpOnly{secure}; SameSite=Strict"
    );
    if let Ok(v) = HeaderValue::from_str(&c) {
        r.headers_mut().append(header::SET_COOKIE, v);
    }
    r.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
}

/// Caps bcrypt work without trusting forwarded addresses; its map is bounded against unauthenticated
/// memory growth.
#[derive(Default)]
pub struct Limiter(Mutex<HashMap<String, (Instant, u32)>>);

impl Limiter {
    pub fn allow(&self, host: &str) -> bool {
        let mut m = self.0.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        m.retain(|_, (start, _)| now.duration_since(*start) < Duration::from_secs(60));
        if !m.contains_key(host) && m.len() >= 1024 {
            return false;
        }
        let e = m.entry(host.to_string()).or_insert((now, 0));
        if e.1 >= 10 {
            return false;
        }
        e.1 += 1;
        true
    }
}

fn too_many() -> Response {
    let mut r = text_error(StatusCode::TOO_MANY_REQUESTS, "Too many attempts");
    r.headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
    r
}

pub async fn registration_status(State(srv): State<Arc<Srv>>) -> Response {
    let Some(auth) = &srv.auth else {
        return text_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Authentication unavailable",
        );
    };
    match auth.registration_open() {
        Ok(open) => {
            let mut r = ok_json(&json!({"registration_enabled": open}));
            r.headers_mut()
                .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            r
        }
        Err(_) => text_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Authentication unavailable",
        ),
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Credentials {
    username: String,
    email: String,
    password: String,
}

fn read_credentials(headers: &HeaderMap, body: &[u8]) -> Result<Credentials, ()> {
    let json_ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("application/json"));
    if !json_ct || body.len() > 4096 {
        return Err(());
    }
    let c: Credentials = serde_json::from_slice(body).map_err(|_| ())?;
    if c.username.is_empty() || c.password.is_empty() || c.password.len() > 72 {
        return Err(());
    }
    Ok(c)
}

pub async fn login(
    State(srv): State<Arc<Srv>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if !srv.limiter.allow(&addr.ip().to_string()) {
        return too_many();
    }
    let Ok(creds) = read_credentials(&headers, &body) else {
        return text_error(StatusCode::BAD_REQUEST, "Invalid request");
    };
    let Some(auth) = srv.auth.clone() else {
        return text_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Authentication unavailable",
        );
    };
    let (u, p) = (creds.username.clone(), creds.password.clone());
    let signed = tokio::task::spawn_blocking(move || auth.login(&u, &p)).await;
    let Ok(Ok(signed)) = signed else {
        return text_error(StatusCode::UNAUTHORIZED, "Invalid credentials");
    };
    let mut r = ok_json(&json!({"username": creds.username}));
    set_session_cookie(&mut r, &signed, SESSION_DURATION_SECS);
    r
}

pub async fn register(
    State(srv): State<Arc<Srv>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if !srv.limiter.allow(&addr.ip().to_string()) {
        return too_many();
    }
    let Ok(creds) = read_credentials(&headers, &body) else {
        return text_error(StatusCode::BAD_REQUEST, "Invalid request");
    };
    let Some(auth) = srv.auth.clone() else {
        return text_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Authentication unavailable",
        );
    };
    let name = creds.username.trim().to_string();
    let (a2, u, e, p) = (
        auth.clone(),
        creds.username.clone(),
        creds.email.clone(),
        creds.password.clone(),
    );
    let reg = tokio::task::spawn_blocking(move || a2.register(&u, &e, &p)).await;
    match reg {
        Ok(Ok(())) => {}
        Ok(Err(AuthError::RegistrationClosed)) => {
            return text_error(StatusCode::FORBIDDEN, "Registration disabled");
        }
        _ => {
            return text_error(
                StatusCode::BAD_REQUEST,
                "Registration failed: username must have 3–128 characters and password 12–72 bytes",
            );
        }
    }
    let (n2, p2) = (name.clone(), creds.password.clone());
    let signed = tokio::task::spawn_blocking(move || auth.login(&n2, &p2)).await;
    let Ok(Ok(signed)) = signed else {
        return internal("Sign in to continue");
    };
    let mut r = json_response(StatusCode::CREATED, &json!({"username": name}));
    set_session_cookie(&mut r, &signed, SESSION_DURATION_SECS);
    r
}

pub async fn user(req: Request) -> Response {
    match req.extensions().get::<Session>() {
        Some(s) => ok_json(&json!({"enabled": true, "username": s.0.sub})),
        None => text_error(StatusCode::UNAUTHORIZED, "Authentication required"),
    }
}

pub async fn logout(State(srv): State<Arc<Srv>>, req: Request) -> Response {
    let Some(session) = req.extensions().get::<Session>().cloned() else {
        return text_error(StatusCode::UNAUTHORIZED, "Authentication required");
    };
    let Some(auth) = srv.auth.clone() else {
        return text_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Authentication unavailable",
        );
    };
    let id = session.0.jti.clone();
    if tokio::task::spawn_blocking(move || auth.revoke(&id))
        .await
        .map_or(true, |r| r.is_err())
    {
        return internal("Logout failed");
    }
    srv.hub.disconnect_session(&session.0.jti);
    let mut r = ok_json(&json!({"success": true}));
    set_session_cookie(&mut r, "", -1);
    r
}

#[cfg(test)]
mod host_tests {
    use super::{MAX_LOGGED_HOSTS, first_rejection, host_allowed_with as ok};
    use std::collections::HashSet;
    use std::sync::Mutex;

    #[test]
    fn rejected_hosts_are_logged_once_and_bounded() {
        let seen = Mutex::new(HashSet::new());
        assert!(first_rejection(&seen, "a.example"));
        assert!(!first_rejection(&seen, "a.example"));
        for i in 0..100 {
            first_rejection(&seen, &format!("h{i}.example"));
        }
        assert_eq!(seen.lock().unwrap().len(), MAX_LOGGED_HOSTS);
        assert!(!first_rejection(&seen, "new.example"));
    }

    #[test]
    fn loopback_ips_and_allowlist() {
        assert!(ok("localhost:8080", ""));
        assert!(ok("127.0.0.1:8080", ""));
        assert!(ok("192.168.1.5", ""));
        assert!(ok("[::1]:8080", ""));
        assert!(!ok("evil.example:8080", ""));
        assert!(!ok("nas.local:8080", ""));
        assert!(ok("nas.local:8080", "nas.local"));
        assert!(ok("NAS.local:8080", "nas.local:8080, other"));
        assert!(!ok("nas.local:9090", "nas.local:8080"));
    }
}
