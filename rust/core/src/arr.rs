//! Port of `internal/arr`: the Sonarr/Radarr/Lidarr/Readarr integration. Go uses the `starr`
//! client library; only three calls are needed (add and delete a Webhook notification, read system
//! status), so they are plain REST calls: `{url}/api/v3/...` for Radarr and Sonarr and
//! `{url}/api/v1/...` for Lidarr and Readarr, authenticated with the `X-Api-Key` header.

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::config::ArrInstance;

pub const NOTIFICATION_NAME: &str = "Postie";

/// Unified import-event payload of the four apps; fields a given app does not send stay empty.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct WebhookPayload {
    #[serde(rename = "eventType")]
    pub event_type: String,
    #[serde(rename = "movieFile", skip_serializing_if = "Option::is_none")]
    pub movie_file: Option<FilePayload>,
    #[serde(rename = "episodeFile", skip_serializing_if = "Option::is_none")]
    pub episode_file: Option<FilePayload>,
    #[serde(rename = "trackFiles", skip_serializing_if = "Vec::is_empty")]
    pub track_files: Vec<FilePayload>,
    #[serde(rename = "bookFiles", skip_serializing_if = "Vec::is_empty")]
    pub book_files: Vec<FilePayload>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct FilePayload {
    pub path: String,
}

/// Final library file paths from a webhook payload. Empty unless the event is `Download`, which the
/// apps fire only after the file has been renamed and moved to its library destination.
pub fn extract_file_paths(p: &WebhookPayload) -> Vec<String> {
    if p.event_type != "Download" {
        return vec![];
    }
    p.movie_file
        .iter()
        .chain(p.episode_file.iter())
        .chain(p.track_files.iter())
        .chain(p.book_files.iter())
        .filter(|f| !f.path.is_empty())
        .map(|f| f.path.clone())
        .collect()
}

#[derive(Debug, thiserror::Error)]
pub enum ArrError {
    #[error("unsupported arr type: {0:?}")]
    UnsupportedType(String),
    #[error("{0}")]
    Other(String),
}

/// API version segment and the "import" flag name per app.
fn app(kind: &str) -> Result<(&'static str, &'static str, &'static str), ArrError> {
    match kind {
        "radarr" => Ok(("radarr", "v3", "onDownload")),
        "sonarr" => Ok(("sonarr", "v3", "onDownload")),
        "lidarr" => Ok(("lidarr", "v1", "onReleaseImport")),
        "readarr" => Ok(("readarr", "v1", "onReleaseImport")),
        other => Err(ArrError::UnsupportedType(other.to_string())),
    }
}

fn endpoint(instance: &ArrInstance, ver: &str, path: &str) -> String {
    format!("{}/api/{ver}/{path}", instance.url.trim_end_matches('/'))
}

/// starr's `ReqError` text for a non-2xx answer.
fn status_error(code: u16, body: &str) -> String {
    let msg = if code >= 300 {
        format!("invalid status code, {code} >= 300")
    } else {
        format!("invalid status code, {code} < 200")
    };
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            let pick = |v: &serde_json::Value| {
                v.get("message")
                    .or_else(|| v.get("errorMessage"))
                    .and_then(|m| m.as_str())
                    .map(str::to_string)
            };
            pick(&v).or_else(|| v.as_array().and_then(|a| a.first()).and_then(pick))
        });
    match (detail, body) {
        (Some(d), _) => format!("{msg}, {d}"),
        (None, b) if b.len() > 400 => format!("{msg}, {}", &b[..400]),
        (None, b) if !b.is_empty() => format!("{msg}, {b}"),
        _ => msg,
    }
}

async fn send(
    client: &reqwest::Client,
    req: reqwest::RequestBuilder,
    key: &str,
) -> Result<String, String> {
    let resp = req
        .header("X-Api-Key", key)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let code = resp.status().as_u16();
    let body = resp.text().await.map_err(|e| e.to_string())?;
    let _ = client;
    if (200..300).contains(&code) {
        Ok(body)
    } else {
        Err(status_error(code, &body))
    }
}

/// Registers a Postie Webhook notification and returns its id (to be saved as `webhook_id`).
/// `webhook_url` is the full Postie URL including the `?apiKey` query parameter.
pub async fn setup_webhook(
    client: &reqwest::Client,
    instance: &ArrInstance,
    webhook_url: &str,
) -> Result<i64, ArrError> {
    let (name, ver, flag) = app(&instance.kind)?;
    let body = json!({
        flag: true,
        "onUpgrade": true,
        "name": NOTIFICATION_NAME,
        "implementation": "Webhook",
        "configContract": "WebhookSettings",
        "fields": [{"name": "url", "value": webhook_url}, {"name": "method", "value": 2}],
    });
    let req = client
        .post(endpoint(instance, ver, "notification"))
        .json(&body);
    let text = send(client, req, &instance.api_key)
        .await
        .map_err(|e| ArrError::Other(format!("{name} AddNotification: {e}")))?;
    let v: serde_json::Value = serde_json::from_str(&text)
        .map_err(|e| ArrError::Other(format!("{name} AddNotification: {e}")))?;
    Ok(v.get("id").and_then(serde_json::Value::as_i64).unwrap_or(0))
}

/// Deletes the Postie notification; a no-op when `webhook_id` is 0.
pub async fn remove_webhook(
    client: &reqwest::Client,
    instance: &ArrInstance,
) -> Result<(), ArrError> {
    if instance.webhook_id == 0 {
        return Ok(());
    }
    let (_, ver, _) = app(&instance.kind)?;
    let req = client.delete(endpoint(
        instance,
        ver,
        &format!("notification/{}", instance.webhook_id),
    ));
    send(client, req, &instance.api_key)
        .await
        .map(|_| ())
        .map_err(ArrError::Other)
}

/// Verifies the instance is reachable and the API key is valid.
pub async fn test_connection(
    client: &reqwest::Client,
    instance: &ArrInstance,
) -> Result<(), ArrError> {
    let (_, ver, _) = app(&instance.kind)?;
    let req = client.get(endpoint(instance, ver, "system/status"));
    send(client, req, &instance.api_key)
        .await
        .map(|_| ())
        .map_err(|e| ArrError::Other(format!("arr connection test failed: {e}")))
}

#[cfg(test)]
mod tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    fn file(p: &str) -> FilePayload {
        FilePayload { path: p.into() }
    }

    #[test]
    fn extracts_paths_for_each_app() {
        let p = WebhookPayload {
            event_type: "Download".into(),
            movie_file: Some(file("/movies/The.Matrix.1999.mkv")),
            ..Default::default()
        };
        assert_eq!(extract_file_paths(&p), ["/movies/The.Matrix.1999.mkv"]);
        let p = WebhookPayload {
            event_type: "Download".into(),
            episode_file: Some(file("/tv/Breaking.Bad.S01E01.mkv")),
            ..Default::default()
        };
        assert_eq!(extract_file_paths(&p), ["/tv/Breaking.Bad.S01E01.mkv"]);
        let p = WebhookPayload {
            event_type: "Download".into(),
            track_files: vec![file("/music/01.flac"), file("/music/02.flac")],
            ..Default::default()
        };
        assert_eq!(extract_file_paths(&p), ["/music/01.flac", "/music/02.flac"]);
        let p = WebhookPayload {
            event_type: "Download".into(),
            book_files: vec![file("/books/Book.epub"), file("")],
            ..Default::default()
        };
        assert_eq!(extract_file_paths(&p), ["/books/Book.epub"]);
    }

    #[test]
    fn non_download_events_are_ignored_and_json_parses() {
        let p = WebhookPayload {
            event_type: "Test".into(),
            movie_file: Some(file("/x")),
            ..Default::default()
        };
        assert!(extract_file_paths(&p).is_empty());
        let p: WebhookPayload = serde_json::from_str(
            r#"{"eventType":"Download","movieFile":{"path":"/m.mkv","other":1},"extra":true}"#,
        )
        .unwrap();
        assert_eq!(extract_file_paths(&p), ["/m.mkv"]);
    }

    /// One canned answer per connection; returns the captured request head and body.
    async fn serve(
        status: &'static str,
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<String>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", l.local_addr().unwrap());
        let h = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = vec![0u8; 16384];
            let mut n = 0;
            loop {
                let m = s.read(&mut buf[n..]).await.unwrap();
                n += m;
                let text = String::from_utf8_lossy(&buf[..n]).to_string();
                if let Some(i) = text.find("\r\n\r\n") {
                    let len = text.to_lowercase().find("content-length: ").map_or(0, |p| {
                        text[p + 16..]
                            .split("\r\n")
                            .next()
                            .unwrap()
                            .trim()
                            .parse::<usize>()
                            .unwrap()
                    });
                    if n >= i + 4 + len {
                        break;
                    }
                }
                if m == 0 {
                    break;
                }
            }
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(resp.as_bytes()).await.unwrap();
            String::from_utf8_lossy(&buf[..n]).to_string()
        });
        (url, h)
    }

    fn instance(url: &str, kind: &str, webhook_id: i64) -> ArrInstance {
        ArrInstance {
            id: "a".into(),
            name: "A".into(),
            kind: kind.into(),
            url: format!("{url}/"),
            api_key: "KEY".into(),
            enabled: true,
            webhook_id,
            delete_after_upload: false,
        }
    }

    #[tokio::test]
    async fn setup_webhook_posts_the_notification_per_app() {
        for (kind, ver, flag) in [
            ("radarr", "v3", "onDownload"),
            ("sonarr", "v3", "onDownload"),
            ("lidarr", "v1", "onReleaseImport"),
            ("readarr", "v1", "onReleaseImport"),
        ] {
            let (url, req) = serve("201 Created", r#"{"id": 42, "name": "Postie"}"#).await;
            let id = setup_webhook(
                &reqwest::Client::new(),
                &instance(&url, kind, 0),
                "http://postie/api/arr/webhook/a?apiKey=k",
            )
            .await
            .unwrap();
            assert_eq!(id, 42, "{kind}");
            let head = req.await.unwrap();
            let lower = head.to_lowercase();
            assert!(
                head.starts_with(&format!("POST /api/{ver}/notification ")),
                "{kind}: {head}"
            );
            assert!(
                lower.contains("x-api-key: key") && lower.contains("application/json"),
                "{kind}"
            );
            let body: serde_json::Value =
                serde_json::from_str(head.split("\r\n\r\n").nth(1).unwrap()).unwrap();
            assert_eq!(body[flag], true);
            assert_eq!(body["onUpgrade"], true);
            assert_eq!(
                (
                    body["name"].as_str(),
                    body["implementation"].as_str(),
                    body["configContract"].as_str()
                ),
                (Some("Postie"), Some("Webhook"), Some("WebhookSettings"))
            );
            assert_eq!(
                body["fields"],
                json!([{"name": "url", "value": "http://postie/api/arr/webhook/a?apiKey=k"}, {"name": "method", "value": 2}])
            );
        }
    }

    #[tokio::test]
    async fn remove_and_test_connection() {
        let c = reqwest::Client::new();
        assert!(
            remove_webhook(&c, &instance("http://127.0.0.1:1", "sonarr", 0))
                .await
                .is_ok(),
            "id 0 is a no-op"
        );
        let (url, req) = serve("200 OK", "{}").await;
        remove_webhook(&c, &instance(&url, "sonarr", 7))
            .await
            .unwrap();
        assert!(
            req.await
                .unwrap()
                .starts_with("DELETE /api/v3/notification/7 ")
        );
        let (url, req) = serve("200 OK", r#"{"version":"4"}"#).await;
        test_connection(&c, &instance(&url, "lidarr", 0))
            .await
            .unwrap();
        assert!(req.await.unwrap().starts_with("GET /api/v1/system/status "));
    }

    #[tokio::test]
    async fn errors_use_go_texts() {
        let c = reqwest::Client::new();
        let (url, _) = serve("401 Unauthorized", r#"{"message":"Unauthorized"}"#).await;
        let e = test_connection(&c, &instance(&url, "radarr", 0))
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(
            e,
            "arr connection test failed: invalid status code, 401 >= 300, Unauthorized"
        );
        let (url, _) = serve(
            "400 Bad Request",
            r#"[{"propertyName":"Url","errorMessage":"Invalid"}]"#,
        )
        .await;
        let e = setup_webhook(&c, &instance(&url, "sonarr", 0), "u")
            .await
            .unwrap_err()
            .to_string();
        assert_eq!(
            e,
            "sonarr AddNotification: invalid status code, 400 >= 300, Invalid"
        );
        assert_eq!(
            test_connection(&c, &instance("http://x", "bogus", 0))
                .await
                .unwrap_err()
                .to_string(),
            "unsupported arr type: \"bogus\""
        );
        assert_eq!(
            remove_webhook(&c, &instance("http://x", "bogus", 3))
                .await
                .unwrap_err()
                .to_string(),
            "unsupported arr type: \"bogus\""
        );
    }
}
