//! Port of `internal/duplicatelookup`: the optional sampled-file duplicate lookup.
//!
//! `fingerprint` hashes size plus three 64 KiB samples (`sample-sha256-v1`); `lookup` POSTs it to
//! a user-configured endpoint and returns the validated NZB, or `None` on an explicit miss (204).
//! NZB validation is the recorded, gate-checked port in `duplicatelookup-rs`.

use std::fs::{File, Metadata};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

pub const ALGORITHM: &str = "sample-sha256-v1";
const MAX_NZB_SIZE: usize = 64 << 20;
const SAMPLE: u64 = 65_536;
const WHOLE_FILE_LIMIT: u64 = 196_608;

/// Error texts match the Go messages.
#[derive(Debug, thiserror::Error)]
pub enum LookupError {
    #[error("duplicate lookup requires a regular file")]
    NotRegular,
    #[error("invalid lookup filename")]
    BadName,
    #[error("source changed during duplicate lookup")]
    SourceChanged,
    #[error("invalid duplicate lookup URL")]
    BadUrl,
    #[error("duplicate lookup timeout must be between 0 and 5 minutes")]
    BadTimeout,
    #[error("duplicate lookup request: {0}")]
    Request(String),
    #[error("duplicate lookup returned HTTP {0}")]
    Http(u16),
    #[error("duplicate lookup response is not application/x-nzb")]
    NotNzb,
    #[error("duplicate lookup NZB exceeds 64 MiB")]
    TooLarge,
    #[error("{0}")]
    Nzb(String),
    #[error("context canceled")]
    Canceled,
    #[error("{0}")]
    Io(#[from] std::io::Error),
}

/// JSON body sent to the lookup endpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Request {
    pub algorithm: String,
    pub name: String,
    pub size: i64,
    pub hash: String,
}

/// Identity of the file's content as the OS sees it (size, mtime, and inode where available).
#[derive(Debug, Clone, PartialEq, Eq)]
struct Stamp {
    len: u64,
    mtime: Option<std::time::SystemTime>,
    #[cfg(unix)]
    id: (u64, u64),
}

fn stamp(m: &Metadata) -> Stamp {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Stamp {
            len: m.len(),
            mtime: m.modified().ok(),
            id: (m.dev(), m.ino()),
        }
    }
    #[cfg(not(unix))]
    {
        Stamp {
            len: m.len(),
            mtime: m.modified().ok(),
        }
    }
}

/// Fingerprint of `path`; checks size/mtime/identity around all sample reads.
pub fn fingerprint(cancel: &CancellationToken, path: &Path) -> Result<Request, LookupError> {
    let mut f = File::open(path)?;
    let before = stamp(&f.metadata()?);
    if !f.metadata()?.is_file() {
        return Err(LookupError::NotRegular);
    }
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if name.contains(['/', '\\']) || name.chars().any(char::is_control) {
        return Err(LookupError::BadName);
    }
    let size = before.len;
    let mut h = Sha256::new();
    h.update(ALGORITHM.as_bytes());
    h.update([0u8]);
    h.update(size.to_le_bytes());
    let (length, offsets): (u64, Vec<u64>) = if size > WHOLE_FILE_LIMIT {
        (SAMPLE, vec![0, (size - SAMPLE) / 2, size - SAMPLE])
    } else {
        (size, vec![0])
    };
    let mut buf = vec![0u8; length as usize];
    for off in offsets {
        if cancel.is_cancelled() {
            return Err(LookupError::Canceled);
        }
        h.update(off.to_le_bytes());
        h.update(length.to_le_bytes());
        f.seek(SeekFrom::Start(off))?;
        f.read_exact(&mut buf)?;
        h.update(&buf);
    }
    let after = stamp(&f.metadata()?);
    let current = stamp(&std::fs::metadata(path)?);
    if before != after || before != current {
        return Err(LookupError::SourceChanged);
    }
    if cancel.is_cancelled() {
        return Err(LookupError::Canceled);
    }
    Ok(Request {
        algorithm: ALGORITHM.into(),
        name,
        size: size as i64,
        hash: hex::encode(h.finalize()),
    })
}

fn check_url(endpoint: &str) -> Result<reqwest::Url, LookupError> {
    // The WHATWG parser accepts `http:///host`; Go's `url.Parse` reads an empty host there, so
    // require a non-empty authority in the text itself.
    let authority = endpoint
        .split_once("://")
        .map(|(_, rest)| rest.split(['/', '?', '#']).next().unwrap_or(""));
    if authority.is_none_or(str::is_empty) {
        return Err(LookupError::BadUrl);
    }
    let u = reqwest::Url::parse(endpoint).map_err(|_| LookupError::BadUrl)?;
    let ok = matches!(u.scheme(), "http" | "https")
        && u.host_str().is_some_and(|h| !h.is_empty())
        && u.username().is_empty()
        && u.password().is_none();
    if ok { Ok(u) } else { Err(LookupError::BadUrl) }
}

/// Looks `path` up at `endpoint`. `Ok(None)` is an explicit miss; every other failure is an error
/// the caller must treat as stopping the upload.
pub async fn lookup(
    cancel: &CancellationToken,
    endpoint: &str,
    key: &str,
    timeout: Duration,
    path: &Path,
) -> Result<Option<Vec<u8>>, LookupError> {
    let url = check_url(endpoint)?;
    if timeout.is_zero() || timeout > Duration::from_secs(300) {
        return Err(LookupError::BadTimeout);
    }
    let deadline = tokio::time::timeout(timeout, run(cancel, url, key, timeout, path));
    match deadline.await {
        Ok(r) => r,
        Err(_) => Err(LookupError::Request("context deadline exceeded".into())),
    }
}

async fn run(
    cancel: &CancellationToken,
    url: reqwest::Url,
    key: &str,
    timeout: Duration,
    path: &Path,
) -> Result<Option<Vec<u8>>, LookupError> {
    let initial = stamp(&std::fs::metadata(path)?);
    let (c2, p2) = (cancel.clone(), path.to_path_buf());
    let fp = tokio::task::spawn_blocking(move || fingerprint(&c2, &p2))
        .await
        .map_err(|e| LookupError::Io(std::io::Error::other(e)))??;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .map_err(|e| LookupError::Request(e.to_string()))?;
    let mut req = client
        .post(url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/x-nzb");
    if !key.is_empty() {
        req = req.header("X-API-Key", key);
    }
    let body = serde_json::to_vec(&fp).map_err(|e| LookupError::Io(std::io::Error::other(e)))?;
    let resp = tokio::select! {
        () = cancel.cancelled() => return Err(LookupError::Canceled),
        r = req.body(body).send() => r.map_err(|e| LookupError::Request(e.to_string()))?,
    };
    let status = resp.status().as_u16();
    if status == 204 {
        return Ok(None);
    }
    if status != 200 {
        return Err(LookupError::Http(status));
    }
    let media = resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .map(|v| v.trim().to_ascii_lowercase());
    if media.as_deref() != Some("application/x-nzb") {
        return Err(LookupError::NotNzb);
    }
    let mut data = Vec::new();
    let mut resp = resp;
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| LookupError::Request(e.to_string()))?
    {
        data.extend_from_slice(&chunk);
        if data.len() > MAX_NZB_SIZE {
            return Err(LookupError::TooLarge);
        }
    }
    duplicatelookup_rs::port::validate_nzb_bytes(&data).map_err(LookupError::Nzb)?;
    let final_stamp = stamp(&std::fs::metadata(path)?);
    if initial != final_stamp {
        return Err(LookupError::SourceChanged);
    }
    if cancel.is_cancelled() {
        return Err(LookupError::Canceled);
    }
    Ok(Some(data))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    use super::*;

    fn pattern(n: usize) -> Vec<u8> {
        (0..n).map(|i| ((i * 7 + 3) % 251) as u8).collect()
    }

    fn file_with(n: usize) -> (tempfile_dir::Dir, std::path::PathBuf) {
        let d = tempfile_dir::Dir::new();
        let p = d.path().join("file.bin");
        File::create(&p).unwrap().write_all(&pattern(n)).unwrap();
        (d, p)
    }

    /// Hashes computed by the Go original (`Fingerprint`) for the same deterministic content.
    #[test]
    fn fingerprint_matches_go_vectors() {
        let vectors = [
            (
                0,
                "05a8b6e0a73e1eea7da7d93b345eb2e99c9e71b6c34e0ae42fde07302a5371d9",
            ),
            (
                1,
                "1199e97f85b5226cb84ef55743a663fe6438cf965bf123938351d3ac0bb15130",
            ),
            (
                100,
                "62fc4f08b5ec08a5ed3ce008d5bc52c86c70d9315c7bba5e83a3c24d086ae83d",
            ),
            (
                196_607,
                "5aa38d76a0a5879e200c56fd2f0f3fd88ce39a7c97e808770c92c85f95a3df04",
            ),
            (
                196_608,
                "58ba255d7e02ca9a8169c3ef8fbb3571dbff6ec94b9fbe9d5ae626a6fa5a9685",
            ),
            (
                196_609,
                "d413e858b27deae6f0753ea6f46618da8ea980c9a42b5161b35e12500323cb85",
            ),
            (
                1_048_576,
                "3fb98ce0e2f6965d107f5c76ea4c52116796e8f4f80cbe42fdc8d7fa50b4ea72",
            ),
        ];
        for (size, want) in vectors {
            let (_d, p) = file_with(size);
            let r = fingerprint(&CancellationToken::new(), &p).unwrap();
            assert_eq!(
                (r.name.as_str(), r.size, r.hash.as_str()),
                ("file.bin", size as i64, want),
                "size {size}"
            );
        }
    }

    #[test]
    fn fingerprint_rejects_directories() {
        let d = tempfile_dir::Dir::new();
        assert!(matches!(
            fingerprint(&CancellationToken::new(), d.path()),
            Err(LookupError::NotRegular)
        ));
    }

    const NZB: &str = r#"<nzb><file subject="s"><groups><group>g</group></groups><segments><segment bytes="1" number="1">id</segment></segments></file></nzb>"#;

    /// Serves one canned HTTP response per connection and captures the request head.
    async fn serve(
        status: &'static str,
        ctype: &'static str,
        body: &'static str,
    ) -> (String, tokio::task::JoinHandle<String>) {
        let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/lookup", l.local_addr().unwrap());
        let h = tokio::spawn(async move {
            let (mut s, _) = l.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let n = s.read(&mut buf).await.unwrap();
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let resp = format!(
                "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            s.write_all(resp.as_bytes()).await.unwrap();
            req
        });
        (url, h)
    }

    #[tokio::test]
    async fn lookup_hit_sends_fingerprint_and_key() {
        let (_d, p) = file_with(100);
        let (url, req) = serve("200 OK", "application/x-nzb; charset=utf-8", NZB).await;
        let got = lookup(
            &CancellationToken::new(),
            &url,
            "secret",
            Duration::from_secs(5),
            &p,
        )
        .await
        .unwrap();
        assert_eq!(got.as_deref(), Some(NZB.as_bytes()));
        let req = req.await.unwrap().to_ascii_lowercase();
        assert!(req.starts_with("post /lookup"), "{req}");
        assert!(req.contains("x-api-key: secret") && req.contains("accept: application/x-nzb"));
        assert!(
            req.contains("\"algorithm\":\"sample-sha256-v1\"")
                && req.contains("\"name\":\"file.bin\"")
        );
    }

    #[tokio::test]
    async fn lookup_miss_and_errors() {
        let (_d, p) = file_with(10);
        let c = CancellationToken::new();
        let (url, _) = serve("204 No Content", "text/plain", "").await;
        assert_eq!(
            lookup(&c, &url, "", Duration::from_secs(5), &p)
                .await
                .unwrap(),
            None
        );
        let (url, _) = serve("500 Internal Server Error", "text/plain", "x").await;
        assert_eq!(
            lookup(&c, &url, "", Duration::from_secs(5), &p)
                .await
                .unwrap_err()
                .to_string(),
            "duplicate lookup returned HTTP 500"
        );
        let (url, _) = serve("200 OK", "text/html", NZB).await;
        assert!(matches!(
            lookup(&c, &url, "", Duration::from_secs(5), &p).await,
            Err(LookupError::NotNzb)
        ));
        let (url, _) = serve("302 Found", "text/plain", "").await;
        assert!(matches!(
            lookup(&c, &url, "", Duration::from_secs(5), &p).await,
            Err(LookupError::Http(302))
        ));
        let (url, _) = serve("200 OK", "application/x-nzb", "<nzb/>").await;
        assert_eq!(
            lookup(&c, &url, "", Duration::from_secs(5), &p)
                .await
                .unwrap_err()
                .to_string(),
            "lookup NZB contains no files"
        );
    }

    #[tokio::test]
    async fn lookup_validates_url_and_timeout() {
        let (_d, p) = file_with(1);
        let c = CancellationToken::new();
        for bad in [
            "ftp://x/y",
            "http://user:pw@host/",
            "http:///nohost",
            "not a url",
        ] {
            assert!(
                matches!(
                    lookup(&c, bad, "", Duration::from_secs(1), &p).await,
                    Err(LookupError::BadUrl)
                ),
                "{bad}"
            );
        }
        for t in [Duration::ZERO, Duration::from_secs(301)] {
            assert!(matches!(
                lookup(&c, "http://127.0.0.1:1/", "", t, &p).await,
                Err(LookupError::BadTimeout)
            ));
        }
    }

    mod tempfile_dir {
        use std::path::{Path, PathBuf};

        pub struct Dir(PathBuf);
        impl Dir {
            pub fn new() -> Self {
                let p = std::env::temp_dir().join(format!(
                    "postie-core-{}-{}",
                    std::process::id(),
                    rand::random::<u64>()
                ));
                std::fs::create_dir_all(&p).unwrap();
                Self(p)
            }
            pub fn path(&self) -> &Path {
                &self.0
            }
        }
        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
