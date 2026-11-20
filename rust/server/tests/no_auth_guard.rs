mod support;
use support::*;

async fn send(
    web: &Web,
    method: reqwest::Method,
    path: &str,
    hdrs: &[(&str, &str)],
) -> (u16, String) {
    let mut r = web.http.request(method, format!("{}{path}", web.base));
    for (k, v) in hdrs {
        r = r.header(*k, *v);
    }
    let resp = r.body("{}").send().await.unwrap();
    (
        resp.status().as_u16(),
        resp.text().await.unwrap_or_default(),
    )
}

#[tokio::test]
async fn no_auth_origin_and_host_guards() {
    let fake = fake_nntp().await;
    let dir = temp_dir("noauthguard");
    let web = Web::start(&dir, &fake).await;
    let evil = [("Origin", "http://evil.example")];

    let (s, body) = send(&web, reqwest::Method::POST, "/api/v1/queue", &evil).await;
    assert!(
        !(s == 403 && body.contains("Cross-origin")),
        "keyed route must skip the origin check: {s} {body}"
    );
    let (s, body) = send(&web, reqwest::Method::POST, "/api/config", &evil).await;
    assert_eq!(s, 403, "{body}");
    assert!(body.contains("Cross-origin"), "{body}");

    let (s, _) = send(
        &web,
        reqwest::Method::GET,
        "/api/status",
        &[("Host", "evil.example")],
    )
    .await;
    assert_eq!(s, 403);
    let host = web.base.trim_start_matches("http://").to_string();
    let (s, _) = send(
        &web,
        reqwest::Method::GET,
        "/api/status",
        &[("Host", host.as_str())],
    )
    .await;
    assert_ne!(s, 403);
    web.stop().await;
}
