//! The frontend's API client and the Rust server must agree: every route the web client calls exists, and
//! every event it listens for has an emitter. These caught nothing at the unit level before.
mod support;
use support::*;

const WEB_CLIENT: &str = include_str!("../../../frontend/src/lib/api/web-client.ts");

/// `(METHOD, path)` for every `this.get/post/delete("/x")`, `API_BASE` fetch and upload in the web client.
fn client_routes() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut push = |m: &str, p: &str| {
        let p = p.split('?').next().unwrap_or(p);
        // `${id}`-style segments become a placeholder
        let mut path = String::new();
        let mut rest = p;
        while let Some(i) = rest.find("${") {
            path.push_str(&rest[..i]);
            path.push('x');
            rest = &rest[rest.find('}').map_or(rest.len(), |j| j + 1)..];
        }
        path.push_str(rest);
        let r = (m.to_string(), format!("/api{path}"));
        // a bare `${API_BASE}${endpoint}` is the client's generic helper, not a route
        if path.len() > 1 && !out.contains(&r) {
            out.push(r);
        }
    };
    let lines: Vec<&str> = WEB_CLIENT.lines().collect();
    for (n, line) in lines.iter().enumerate() {
        // a fetch's options sit on the next lines
        let window = lines[n..(n + 4).min(lines.len())].join(" ");
        for m in ["get", "post", "put", "delete"] {
            for q in ['"', '`', '\''] {
                let pat = format!("this.{m}(");
                let pat2 = format!("this.{m}<");
                for needle in [&pat, &pat2] {
                    let mut at = 0;
                    while let Some(i) = line[at..].find(needle.as_str()) {
                        let rest = &line[at + i..];
                        if let Some(s) = rest.find(q) {
                            let body = &rest[s + 1..];
                            if let Some(e) = body.find(q)
                                && body.starts_with('/')
                            {
                                push(&m.to_uppercase(), &body[..e]);
                            }
                        }
                        at += i + needle.len();
                    }
                }
            }
        }
        if let Some(i) = line.find("${API_BASE}/") {
            let rest = &line[i + "${API_BASE}".len()..];
            let end = rest.find(['`', '"', '\'']).unwrap_or(rest.len());
            let m = if window.contains("method: \"POST\"") || window.contains("xhr.open(\"POST\"") {
                "POST"
            } else if window.contains("method: \"DELETE\"") {
                "DELETE"
            } else if window.contains("method: \"PUT\"") {
                "PUT"
            } else {
                "GET"
            };
            push(m, &rest[..end]);
        }
    }
    out
}

/// Client methods no component calls; neither the Go server nor this one implements them.
const KNOWN_DEAD: [&str; 2] = ["/api/test-provider-connectivity", "/api/config/applied"];

#[test]
fn the_route_extraction_finds_the_known_endpoints() {
    let r = client_routes();
    for (m, p) in [
        ("GET", "/api/queue/stats"),
        ("GET", "/api/running-jobs"),
        ("POST", "/api/config"),
        ("DELETE", "/api/queue/x/cancel"),
        ("POST", "/api/upload"),
    ] {
        assert!(r.contains(&(m.into(), p.into())), "{m} {p} in {r:?}");
    }
    assert!(r.len() >= 30, "{} routes: {r:?}", r.len());
}

#[tokio::test]
async fn every_route_the_web_client_calls_exists_on_the_server() {
    let fake = fake_nntp().await;
    let dir = temp_dir("contract");
    let web = Web::start(&dir, &fake).await;
    let mut missing = Vec::new();
    for (m, p) in client_routes() {
        if KNOWN_DEAD.contains(&p.as_str()) {
            continue;
        }
        let url = format!("{}{p}", web.base);
        let req = match m.as_str() {
            "POST" => web.http.post(&url).json(&serde_json::json!({})),
            "DELETE" => web.http.delete(&url),
            "PUT" => web.http.put(&url).json(&serde_json::json!({})),
            _ => web.http.get(&url),
        };
        let r = req.send().await.unwrap();
        let (status, ctype) = (
            r.status(),
            r.headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string(),
        );
        // an existing handler may refuse the empty request (400, 409, 500...); a missing route is a 404, a
        // method mismatch a 405, and the static fallback answers HTML
        if status == 404 || status == 405 || status.is_redirection() || ctype.contains("html") {
            missing.push(format!("{m} {p} -> {status} {ctype}"));
        }
    }
    web.stop().await;
    assert!(
        missing.is_empty(),
        "routes the frontend calls but the server lacks:\n{}",
        missing.join("\n")
    );
}

/// Events the frontend subscribes to over the websocket in web mode, and the Go app's full emit set.
/// Every one needs an emitter in the Rust core or server (`config-applied` has none in Go either).
#[test]
fn every_event_the_ui_listens_for_has_an_emitter() {
    let sources = [
        include_str!("../../core/src/backend/app.rs"),
        include_str!("../../core/src/backend/app/api.rs"),
        include_str!("../src/routes.rs"),
    ]
    .join("\n");
    for ev in [
        "queue-updated",
        "running-jobs-updated",
        "nntp-pool-metrics-updated",
        "processing:paused",
        "processing:resumed",
        "processing:auto-paused",
        "job-error",
        "config-updated",
        "config-pending",
        "upload-progress",
        "upload-complete",
        "upload-error",
        "import-progress",
        "import-complete",
        "import-error",
    ] {
        assert!(
            sources.contains(&format!("\"{ev}\"")),
            "no emitter for `{ev}`"
        );
    }
}
