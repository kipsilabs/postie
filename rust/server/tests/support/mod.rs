//! Test harness: a scriptable in-process NNTP server and a real `postie-web` over a temp directory.
//! Nothing here touches the user's HOME, config or queue.
#![allow(dead_code)]

pub mod download;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use postie_core::backend::app::{App, NntpTester};
use postie_core::backend::paths::AppPaths;
use postie_core::config::{self, ConfigData, ServerConfig};
use postie_web::{Running, Srv};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PostMode {
    Accept,
    /// Reply with this error code.
    Reject(u16),
    /// Close the connection after reading the body, without a reply.
    DropAfterBody,
    /// Read the body and never answer.
    Hang,
    /// Accept, but only after this many milliseconds.
    Delay(u64),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum StatMode {
    /// 223 for articles that were posted, 430 otherwise.
    Stored,
    Missing,
    Code(u16),
}

pub struct Ctl {
    pub post: Mutex<PostMode>,
    pub stat: Mutex<StatMode>,
    pub stored: Mutex<HashSet<String>>,
    pub posts: AtomicUsize,
    pub stats: AtomicUsize,
    pub conns: AtomicUsize,
    /// Fail the AUTHINFO PASS exchange.
    pub bad_auth: Mutex<bool>,
    /// Every accepted article as a server would keep it (headers + body, dot-unstuffed), by Message-ID.
    pub articles: Mutex<HashMap<String, Vec<u8>>>,
    pub faults: Mutex<Faults>,
}

/// One-off failures picked by the 1-based number of the POST that receives them, so a test injects
/// the same number of faults however the uploads interleave.
#[derive(Default, Debug)]
pub struct Faults {
    /// Answer 240 but keep nothing, as an article lost to propagation.
    pub lose: HashSet<usize>,
    /// Answer 441.
    pub reject: HashSet<usize>,
    /// Close the connection without answering.
    pub drop: HashSet<usize>,
}

#[derive(PartialEq)]
enum Fault {
    None,
    Lose,
    Reject,
    Drop,
}

pub struct Fake {
    pub addr: std::net::SocketAddr,
    pub ctl: Arc<Ctl>,
}

impl Fake {
    pub fn set_post(&self, m: PostMode) {
        *self.ctl.post.lock().unwrap() = m;
    }
    pub fn set_stat(&self, m: StatMode) {
        *self.ctl.stat.lock().unwrap() = m;
    }
    pub fn posts(&self) -> usize {
        self.ctl.posts.load(Ordering::SeqCst)
    }
    pub fn stats(&self) -> usize {
        self.ctl.stats.load(Ordering::SeqCst)
    }
    /// Injects faults into the POSTs numbered in each list (counting every POST received so far).
    pub fn inject(&self, lose: &[usize], reject: &[usize], drop: &[usize]) {
        let mut f = self.ctl.faults.lock().unwrap();
        f.lose.extend(lose);
        f.reject.extend(reject);
        f.drop.extend(drop);
    }
    pub fn article(&self, id: &str) -> Option<Vec<u8>> {
        self.ctl.articles.lock().unwrap().get(id).cloned()
    }
    pub fn article_ids(&self) -> Vec<String> {
        self.ctl.articles.lock().unwrap().keys().cloned().collect()
    }
}

pub async fn fake_nntp() -> Fake {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let ctl = Arc::new(Ctl {
        post: Mutex::new(PostMode::Accept),
        stat: Mutex::new(StatMode::Stored),
        stored: Mutex::new(HashSet::new()),
        posts: AtomicUsize::new(0),
        stats: AtomicUsize::new(0),
        conns: AtomicUsize::new(0),
        bad_auth: Mutex::new(false),
        articles: Mutex::new(HashMap::new()),
        faults: Mutex::new(Faults::default()),
    });
    let c2 = ctl.clone();
    tokio::spawn(async move {
        loop {
            let Ok((s, _)) = l.accept().await else { return };
            c2.conns.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(conn(s, c2.clone()));
        }
    });
    Fake { addr, ctl }
}

async fn conn(s: tokio::net::TcpStream, ctl: Arc<Ctl>) {
    let (r, mut w) = s.into_split();
    let mut r = BufReader::new(r);
    let _ = w.write_all(b"200 fake ready\r\n").await;
    let mut raw = Vec::new();
    loop {
        raw.clear();
        if r.read_until(b'\n', &mut raw).await.unwrap_or(0) == 0 {
            return;
        }
        let cmd = String::from_utf8_lossy(&raw).trim_end().to_string();
        let up = cmd.to_uppercase();
        if std::env::var_os("FAKE_TRACE").is_some() {
            eprintln!("fake <- {}", &cmd[..cmd.len().min(60)]);
        }
        let reply: String = if up.starts_with("AUTHINFO USER") {
            "381 pass\r\n".into()
        } else if up.starts_with("AUTHINFO PASS") {
            if *ctl.bad_auth.lock().unwrap() {
                "481 bad credentials\r\n".into()
            } else {
                "281 ok\r\n".into()
            }
        } else if up.starts_with("STAT ") {
            ctl.stats.fetch_add(1, Ordering::SeqCst);
            let id = cmd[5..].trim().trim_matches(['<', '>']).to_string();
            let mode = *ctl.stat.lock().unwrap();
            match mode {
                StatMode::Code(c) => format!("{c} nope\r\n"),
                StatMode::Missing => "430 no such article\r\n".into(),
                StatMode::Stored if ctl.stored.lock().unwrap().contains(&id) => {
                    "223 0 x\r\n".into()
                }
                StatMode::Stored => "430 no such article\r\n".into(),
            }
        } else if up == "POST" {
            let _ = w.write_all(b"340 go\r\n").await;
            let mut id = String::new();
            let mut article = Vec::new();
            loop {
                raw.clear();
                // yEnc bodies are not UTF-8
                if r.read_until(b'\n', &mut raw).await.unwrap_or(0) == 0 {
                    return;
                }
                if raw == b".\r\n" {
                    break;
                }
                if let Some(v) = raw.strip_prefix(b"Message-ID:") {
                    id = String::from_utf8_lossy(v)
                        .trim()
                        .trim_matches(['<', '>'])
                        .to_string();
                }
                // RFC 3977 dot-stuffing: the server keeps the line without the extra leading dot
                let line = if raw.starts_with(b"..") {
                    &raw[1..]
                } else {
                    &raw[..]
                };
                article.extend_from_slice(line);
            }
            let n = ctl.posts.fetch_add(1, Ordering::SeqCst) + 1;
            let fault = {
                let mut f = ctl.faults.lock().unwrap();
                if f.lose.remove(&n) {
                    Fault::Lose
                } else if f.reject.remove(&n) {
                    Fault::Reject
                } else if f.drop.remove(&n) {
                    Fault::Drop
                } else {
                    Fault::None
                }
            };
            let mode = *ctl.post.lock().unwrap();
            match (fault, mode) {
                (Fault::Drop, _) | (_, PostMode::DropAfterBody) => return,
                (Fault::Reject, _) => "441 rejected\r\n".into(),
                (Fault::Lose, _) => "240 posted\r\n".into(),
                (Fault::None, PostMode::Accept | PostMode::Delay(_)) => {
                    if let PostMode::Delay(ms) = mode {
                        tokio::time::sleep(Duration::from_millis(ms)).await;
                    }
                    ctl.stored.lock().unwrap().insert(id.clone());
                    ctl.articles.lock().unwrap().insert(id, article);
                    "240 posted\r\n".into()
                }
                (Fault::None, PostMode::Reject(c)) => format!("{c} rejected\r\n"),
                (Fault::None, PostMode::Hang) => {
                    std::future::pending::<()>().await;
                    return;
                }
            }
        } else if up == "QUIT" {
            let _ = w.write_all(b"205 bye\r\n").await;
            return;
        } else if up.starts_with("GROUP ") || up.starts_with("MODE ") {
            "211 0 0 0 x\r\n".into()
        } else {
            "500 unknown\r\n".into()
        };
        if w.write_all(reply.as_bytes()).await.is_err() {
            return;
        }
    }
}

pub struct Web {
    pub base: String,
    pub app: Arc<App>,
    pub dir: PathBuf,
    pub http: reqwest::Client,
    running: Running,
}

pub fn temp_dir(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "postie-it-{tag}-{}-{}",
        std::process::id(),
        rand::random::<u32>()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn paths(dir: &Path) -> AppPaths {
    std::fs::create_dir_all(dir.join("config")).unwrap();
    std::fs::create_dir_all(dir.join("data")).unwrap();
    AppPaths {
        config: dir.join("config/config.yaml"),
        database: dir.join("config/postie.db"),
        data: dir.join("data"),
        log: dir.join("data/postie.log"),
    }
}

/// A config that posts to `port` with fast retries and verification, so failure paths finish in seconds.
pub fn fast_config(dir: &Path, port: u16) -> ConfigData {
    let mut c = config::default_config();
    c.database.database_path = dir.join("config/postie.db").to_string_lossy().into_owned();
    c.output_dir = dir.join("out").to_string_lossy().into_owned();
    c.servers = Some(vec![ServerConfig {
        host: "127.0.0.1".into(),
        port: i64::from(port),
        role: "upload".into(),
        max_connections: 2,
        ..Default::default()
    }]);
    c.posting.max_retries = 1;
    c.posting.retry_delay = config::Duration::new("100ms");
    c.posting.article_size_in_bytes = 100_000;
    c.par2.enabled = Some(false);
    c.post_check.retry_delay = config::Duration::new("200ms");
    c.post_check.max_repost = 1;
    c.post_check.deferred_check_delay = config::Duration::new("300ms");
    c.post_check.deferred_max_retries = 2;
    c.post_check.deferred_max_backoff = config::Duration::new("500ms");
    c.post_check.deferred_check_interval = config::Duration::new("300ms");
    c
}

impl Web {
    /// Starts the app and the HTTP server in `dir`, configured for `fake`.
    pub async fn start(dir: &Path, fake: &Fake) -> Web {
        let cfg = fast_config(dir, fake.addr.port());
        Self::start_with(dir, cfg).await
    }

    pub async fn start_with(dir: &Path, cfg: ConfigData) -> Web {
        let app = App::new(paths(dir), Arc::new(NntpTester));
        app.set_web_mode(true);
        app.startup();
        app.save_config(cfg).await.expect("save config");
        Self::serve(dir, app).await
    }

    /// Starts the server over an app that has already started (a restart over an existing database).
    pub async fn restart(dir: &Path) -> Web {
        let app = App::new(paths(dir), Arc::new(NntpTester));
        app.set_web_mode(true);
        app.startup();
        Self::serve(dir, app).await
    }

    /// Like `start`, with web authentication switched on (as `--auth`).
    pub async fn start_with_auth(dir: &Path, fake: &Fake) -> Web {
        let cfg = fast_config(dir, fake.addr.port());
        let app = App::new(paths(dir), Arc::new(NntpTester));
        app.set_web_mode(true);
        app.startup();
        app.save_config(cfg).await.expect("save config");
        let db_path = dir.join("config/postie.db");
        let db = postie_core::database::open("sqlite", &db_path.to_string_lossy()).unwrap();
        postie_core::database::MigrationRunner::new(db.clone())
            .ensure_migration_compatibility()
            .unwrap();
        let auth = Arc::new(postie_core::webauth::Service::new(db).unwrap());
        Self::serve_with(dir, app, Some(auth), true).await
    }

    async fn serve(dir: &Path, app: Arc<App>) -> Web {
        Self::serve_with(dir, app, None, false).await
    }

    async fn serve_with(
        dir: &Path,
        app: Arc<App>,
        auth: Option<Arc<postie_core::webauth::Service>>,
        required: bool,
    ) -> Web {
        let srv = Srv::new(app.clone(), auth, required, PathBuf::from("/nonexistent"));
        let running = postie_web::serve(srv, "127.0.0.1:0").await.unwrap();
        Web {
            base: format!("http://{}", running.addr),
            app,
            dir: dir.to_path_buf(),
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            running,
        }
    }

    pub async fn stop(self) {
        self.running.shutdown.cancel();
        let _ = self.running.task.await;
        self.app.shutdown().await;
    }

    pub async fn get(&self, path: &str) -> Value {
        let r = self
            .http
            .get(format!("{}{path}", self.base))
            .send()
            .await
            .unwrap();
        assert!(r.status().is_success(), "GET {path}: {}", r.status());
        r.json().await.unwrap_or(Value::Null)
    }

    pub async fn post_json(&self, path: &str, body: &Value) -> reqwest::Response {
        self.http
            .post(format!("{}{path}", self.base))
            .json(body)
            .send()
            .await
            .unwrap()
    }

    /// Multipart upload of `(name, bytes)` to `/api/upload`.
    pub async fn upload(&self, files: Vec<(String, Vec<u8>)>) -> reqwest::Response {
        let mut form = reqwest::multipart::Form::new();
        for (n, b) in files {
            form = form.part("files", reqwest::multipart::Part::bytes(b).file_name(n));
        }
        self.http
            .post(format!("{}/api/upload", self.base))
            .multipart(form)
            .send()
            .await
            .unwrap()
    }

    pub async fn queue_items(&self) -> Vec<Value> {
        self.get("/api/queue?limit=100").await["items"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    pub async fn wait_status(&self, name: &str, status: &str, secs: u64) -> Value {
        wait(&format!("{name} -> {status}"), secs, || async {
            self.queue_items()
                .await
                .into_iter()
                .find(|i| i["fileName"] == name && i["status"] == status)
        })
        .await
    }

    /// Opens the WebSocket and collects every event as `(type, data)`.
    pub async fn events(&self) -> Events {
        let url = format!("ws{}/api/ws", &self.base["http".len()..]);
        let (ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        let log = Arc::new(Mutex::new(Vec::<(String, Value)>::new()));
        let l2 = log.clone();
        let (_tx, mut rx) = ws.split();
        tokio::spawn(async move {
            let _keep = _tx;
            while let Some(Ok(m)) = rx.next().await {
                if let Ok(t) = m.to_text()
                    && let Ok(v) = serde_json::from_str::<Value>(t)
                {
                    l2.lock().unwrap().push((
                        v["type"].as_str().unwrap_or("").to_string(),
                        v["data"].clone(),
                    ));
                }
            }
        });
        Events(log)
    }
}

pub struct Events(Arc<Mutex<Vec<(String, Value)>>>);

impl Events {
    pub fn all(&self) -> Vec<(String, Value)> {
        self.0.lock().unwrap().clone()
    }
    pub fn kinds(&self) -> Vec<String> {
        self.all().into_iter().map(|e| e.0).collect()
    }
    pub fn count(&self, kind: &str) -> usize {
        self.kinds().iter().filter(|k| *k == kind).count()
    }
    pub async fn wait_for(&self, kind: &str, secs: u64) {
        let end = Instant::now() + Duration::from_secs(secs);
        while self.count(kind) == 0 {
            assert!(
                Instant::now() < end,
                "no `{kind}` event within {secs}s; saw {:?}",
                self.kinds()
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

/// Deterministic file content that does not compress away.
pub fn blob(len: usize) -> Vec<u8> {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect()
}

/// Waits until `f` returns Some, polling every 50 ms; panics with `what` after `secs`.
pub async fn wait<T, Fut: std::future::Future<Output = Option<T>>>(
    what: &str,
    secs: u64,
    f: impl Fn() -> Fut,
) -> T {
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        if let Some(v) = f().await {
            return v;
        }
        assert!(Instant::now() < end, "timed out waiting for: {what}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
