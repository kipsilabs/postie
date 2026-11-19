//! Behavior tests against an in-process fake NNTP server.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use postie_nntp::pool::{Client, ProviderConfig, ProviderPool};
use postie_nntp::{Endpoint, NntpError, Socks5};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

#[derive(Clone)]
struct Behavior {
    auth: Option<(String, String)>,
    /// Hold STAT replies until this many commands arrived (0 = reply at once).
    hold_stats: usize,
    present: fn(&str) -> bool,
    post_reply: fn(&[u8]) -> (u16, &'static str),
    stat_code: Option<u16>,
    /// Drop the connection right after the first POST body (no reply).
    drop_first_post: bool,
    tls: bool,
}

impl Default for Behavior {
    fn default() -> Self {
        Self {
            auth: None,
            hold_stats: 0,
            present: |_| true,
            post_reply: |_| (240, "posted"),
            stat_code: None,
            drop_first_post: false,
            tls: false,
        }
    }
}

#[derive(Default)]
struct Recorded {
    posts: Vec<Vec<u8>>,
    stats: Vec<String>,
    connections: usize,
    dropped_once: bool,
}

struct Fake {
    addr: std::net::SocketAddr,
    rec: Arc<Mutex<Recorded>>,
}

impl Fake {
    fn endpoint(&self) -> Endpoint {
        Endpoint::new("127.0.0.1", self.addr.port())
    }
    fn posts(&self) -> Vec<Vec<u8>> {
        self.rec.lock().unwrap().posts.clone()
    }
    fn connections(&self) -> usize {
        self.rec.lock().unwrap().connections
    }
}

fn tls_acceptor() -> tokio_rustls::TlsAcceptor {
    let ck = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let cert = ck.cert.der().clone();
    let key = rustls::pki_types::PrivateKeyDer::try_from(ck.signing_key.serialize_der()).unwrap();
    let cfg = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert], key)
    .unwrap();
    tokio_rustls::TlsAcceptor::from(Arc::new(cfg))
}

async fn serve_conn<S: AsyncRead + AsyncWrite + Unpin>(
    s: S,
    b: Behavior,
    rec: Arc<Mutex<Recorded>>,
) {
    let (r, mut w) = tokio::io::split(s);
    let mut r = BufReader::new(r);
    let _ = w.write_all(b"200 fake ready\r\n").await;
    let mut held = 0usize;
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line).await.unwrap_or(0) == 0 {
            return;
        }
        let cmd = line.trim_end().to_string();
        let up = cmd.to_uppercase();
        if up.starts_with("AUTHINFO USER") {
            if b.auth.is_some() {
                let _ = w.write_all(b"381 pass\r\n").await;
            } else {
                let _ = w.write_all(b"281 ok\r\n").await;
            }
        } else if up.starts_with("AUTHINFO PASS") {
            let (u, p) = b.auth.clone().unwrap_or_default();
            let ok = cmd.ends_with(&p) && !p.is_empty();
            let _ = u;
            let _ = w
                .write_all(if ok {
                    b"281 ok\r\n" as &[u8]
                } else {
                    b"481 bad\r\n"
                })
                .await;
        } else if up.starts_with("STAT ") {
            let id = cmd[5..].trim_matches(['<', '>']).to_string();
            rec.lock().unwrap().stats.push(id.clone());
            held += 1;
            if b.hold_stats == 0 || held >= b.hold_stats {
                let n = if b.hold_stats == 0 { 1 } else { held };
                let reply = match b.stat_code {
                    Some(c) => format!("{c} nope\r\n"),
                    None if (b.present)(&id) => "223 0 x\r\n".to_string(),
                    None => "430 no such article\r\n".to_string(),
                };
                let _ = w.write_all(reply.repeat(n).as_bytes()).await;
                held = 0;
            }
        } else if up == "POST" {
            let _ = w.write_all(b"340 go\r\n").await;
            let mut body = Vec::new();
            loop {
                line.clear();
                if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                    return;
                }
                if line == ".\r\n" {
                    break;
                }
                body.extend_from_slice(line.as_bytes());
            }
            rec.lock().unwrap().posts.push(body.clone());
            if b.drop_first_post && !std::mem::replace(&mut rec.lock().unwrap().dropped_once, true)
            {
                return;
            }
            let (code, text) = (b.post_reply)(&body);
            let _ = w.write_all(format!("{code} {text}\r\n").as_bytes()).await;
        } else if up == "QUIT" {
            let _ = w.write_all(b"205 bye\r\n").await;
            return;
        } else {
            let _ = w.write_all(b"500 unknown\r\n").await;
        }
    }
}

async fn fake(b: Behavior) -> Fake {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let rec = Arc::new(Mutex::new(Recorded::default()));
    let acceptor = b.tls.then(tls_acceptor);
    let rec2 = rec.clone();
    tokio::spawn(async move {
        loop {
            let Ok((s, _)) = l.accept().await else { return };
            rec2.lock().unwrap().connections += 1;
            let (b, rec) = (b.clone(), rec2.clone());
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                match acceptor {
                    Some(a) => {
                        if let Ok(t) = a.accept(s).await {
                            serve_conn(t, b, rec).await;
                        }
                    }
                    None => serve_conn(s, b, rec).await,
                }
            });
        }
    });
    Fake { addr, rec }
}

fn cfg(f: &Fake) -> ProviderConfig {
    let mut c = ProviderConfig::new(f.endpoint());
    c.connect_backoff = Duration::from_millis(20);
    c.connections = 2;
    c
}

fn article(n: usize) -> Vec<u8> {
    format!("Subject: s{n}\r\n\r\n=ybegin part=1 total=1 line=128 size=3 name=f\r\n=ypart begin=1 end=3\r\nabc\r\n=yend size=3 part=1 pcrc32=00000000\r\n").into_bytes()
}

#[tokio::test]
async fn post_and_stat_roundtrip_with_lazy_connect() {
    let f = fake(Behavior {
        present: |id| id == "here@x",
        ..Default::default()
    })
    .await;
    let pool = ProviderPool::start(cfg(&f), None);
    assert_eq!(f.connections(), 0, "no connection before the first request");
    pool.post(article(1)).await.unwrap();
    assert!(pool.stat("here@x").await.unwrap());
    assert!(!pool.stat("gone@x").await.unwrap());
    assert_eq!(f.posts(), vec![article(1)]);
    let s = pool.stats();
    assert_eq!(
        (s.posted, s.stat_present, s.stat_missing, s.connects),
        (1, 1, 1, 1)
    );
}

#[tokio::test]
async fn stats_are_pipelined_within_the_window() {
    // the server answers only after 5 STATs arrived: impossible without pipelining
    let f = fake(Behavior {
        hold_stats: 5,
        ..Default::default()
    })
    .await;
    let mut c = cfg(&f);
    c.connections = 1;
    c.inflight = 10;
    let pool = ProviderPool::start(c, None);
    let hs: Vec<_> = (0..5)
        .map(|i| {
            let p = pool.clone();
            tokio::spawn(async move { p.stat(&format!("id{i}@x")).await })
        })
        .collect();
    for h in hs {
        assert!(
            tokio::time::timeout(Duration::from_secs(3), h)
                .await
                .expect("pipelined")
                .unwrap()
                .unwrap()
        );
    }

    // with a window of 1 the same server never gets 5 commands at once
    let f = fake(Behavior {
        hold_stats: 5,
        ..Default::default()
    })
    .await;
    let mut c = cfg(&f);
    c.connections = 1;
    c.inflight = 1;
    let pool = ProviderPool::start(c, None);
    let p = pool.clone();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(400),
            async move { p.stat("a@x").await }
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn post_is_two_phase_and_never_interleaved_with_other_commands() {
    let f = fake(Behavior::default()).await;
    let mut c = cfg(&f);
    c.connections = 1;
    let pool = ProviderPool::start(c, None);
    let mut hs = Vec::new();
    for i in 0..8 {
        let p = pool.clone();
        hs.push(tokio::spawn(async move { p.post(article(i)).await }));
        let p = pool.clone();
        hs.push(tokio::spawn(async move {
            p.stat(&format!("s{i}@x")).await.map(|_| ())
        }));
    }
    for h in hs {
        h.await.unwrap().unwrap();
    }
    let posts = f.posts();
    assert_eq!(posts.len(), 8);
    for body in &posts {
        let s = String::from_utf8_lossy(body);
        assert!(
            !s.contains("STAT ") && !s.contains("POST"),
            "command leaked into an article body: {s}"
        );
    }
    let mut got: Vec<_> = posts
        .iter()
        .map(|b| String::from_utf8_lossy(b).to_string())
        .collect();
    got.sort();
    let mut want: Vec<_> = (0..8)
        .map(|i| String::from_utf8(article(i)).unwrap())
        .collect();
    want.sort();
    assert_eq!(got, want);
    assert_eq!(f.connections(), 1);
}

#[tokio::test]
async fn rejections_and_unexpected_codes_are_errors_not_missing() {
    let f = fake(Behavior {
        post_reply: |_| (441, "posting failed"),
        stat_code: Some(500),
        ..Default::default()
    })
    .await;
    let pool = ProviderPool::start(cfg(&f), None);
    assert_eq!(
        pool.post(article(1)).await,
        Err(NntpError::PostRejected {
            code: 441,
            text: "posting failed".into()
        })
    );
    assert!(matches!(
        pool.stat("a@x").await,
        Err(NntpError::Protocol { code: 500, .. })
    ));
}

#[tokio::test]
async fn a_dropped_connection_fails_in_flight_requests_and_the_pool_recovers() {
    let f = fake(Behavior {
        drop_first_post: true,
        ..Default::default()
    })
    .await;
    let mut c = cfg(&f);
    c.connections = 1;
    let pool = ProviderPool::start(c, None);
    assert_eq!(pool.post(article(1)).await, Err(NntpError::Disconnected));
    pool.post(article(2)).await.unwrap();
    assert_eq!(f.connections(), 2);
    assert!(NntpError::Disconnected.is_retryable());
}

#[tokio::test]
async fn authentication_success_and_failure() {
    let f = fake(Behavior {
        auth: Some(("user".into(), "secret".into())),
        ..Default::default()
    })
    .await;
    let mut ep = f.endpoint();
    ep.username = "user".into();
    ep.password = "secret".into();
    let mut c = ProviderConfig::new(ep.clone());
    c.connect_backoff = Duration::from_millis(10);
    ProviderPool::start(c, None).post(article(1)).await.unwrap();

    ep.password = "wrong".into();
    let mut c = ProviderConfig::new(ep);
    c.connect_backoff = Duration::from_millis(10);
    let e = ProviderPool::start(c, None)
        .post(article(1))
        .await
        .unwrap_err();
    assert!(matches!(e, NntpError::Auth { code: 481, .. }), "{e:?}");
}

#[tokio::test]
async fn idle_connections_close_and_reopen() {
    let f = fake(Behavior::default()).await;
    let mut c = cfg(&f);
    c.connections = 1;
    c.idle_timeout = Duration::from_millis(100);
    let pool = ProviderPool::start(c, None);
    pool.post(article(1)).await.unwrap();
    assert_eq!(pool.stats().active_connections, 1);
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(
        pool.stats().active_connections,
        0,
        "idle connection should have closed"
    );
    pool.post(article(2)).await.unwrap();
    assert_eq!(pool.stats().connects, 2);
}

#[tokio::test]
async fn throttle_paces_aggregate_post_bytes() {
    let f = fake(Behavior::default()).await;
    let mk = || {
        let mut v = b"Subject: s\r\n\r\n".to_vec();
        v.extend(std::iter::repeat_n(b'a', 20_000));
        v.extend_from_slice(b"\r\n");
        v
    };
    let client = Arc::new(Client::from_configs(vec![cfg(&f)], 100_000)); // 100 kB/s
    let start = Instant::now();
    let posts: Vec<_> = (0..3)
        .map(|_| {
            let (c, body) = (client.clone(), mk());
            tokio::spawn(async move { c.post(body).await })
        })
        .collect();
    for p in posts {
        p.await.unwrap().unwrap();
    }
    // 3 x 20 kB at 100 kB/s, sent concurrently: the third body still waits ~0.4 s
    assert!(
        start.elapsed() >= Duration::from_millis(350),
        "{:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn tls_insecure_works_and_verification_rejects_self_signed() {
    let f = fake(Behavior {
        tls: true,
        ..Default::default()
    })
    .await;
    let mut ep = Endpoint::new("localhost", f.addr.port());
    ep.tls = true;
    ep.insecure_tls = true;
    let mut c = ProviderConfig::new(ep.clone());
    c.connect_backoff = Duration::from_millis(10);
    ProviderPool::start(c, None).post(article(1)).await.unwrap();
    assert_eq!(f.posts().len(), 1);

    ep.insecure_tls = false;
    let mut c = ProviderConfig::new(ep);
    c.connect_backoff = Duration::from_millis(10);
    c.connect_attempts = 1;
    let e = ProviderPool::start(c, None)
        .post(article(1))
        .await
        .unwrap_err();
    assert!(
        matches!(e, NntpError::Connect(m) if m.contains("TLS")),
        "self-signed certificate must be rejected"
    );
}

/// A minimal SOCKS5 server (no auth) that tunnels to the requested target.
async fn socks_server() -> (std::net::SocketAddr, Arc<Mutex<Vec<String>>>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut c, _)) = l.accept().await else {
                return;
            };
            let seen = seen2.clone();
            tokio::spawn(async move {
                let mut hdr = [0u8; 2];
                c.read_exact(&mut hdr).await.unwrap();
                let mut methods = vec![0u8; hdr[1] as usize];
                c.read_exact(&mut methods).await.unwrap();
                c.write_all(&[5, 0]).await.unwrap();
                let mut req = [0u8; 4];
                c.read_exact(&mut req).await.unwrap();
                let host = match req[3] {
                    3 => {
                        let mut len = [0u8; 1];
                        c.read_exact(&mut len).await.unwrap();
                        let mut h = vec![0u8; len[0] as usize];
                        c.read_exact(&mut h).await.unwrap();
                        String::from_utf8(h).unwrap()
                    }
                    1 => {
                        let mut ip = [0u8; 4];
                        c.read_exact(&mut ip).await.unwrap();
                        std::net::Ipv4Addr::from(ip).to_string()
                    }
                    _ => panic!("unsupported atyp"),
                };
                let mut port = [0u8; 2];
                c.read_exact(&mut port).await.unwrap();
                let port = u16::from_be_bytes(port);
                seen.lock().unwrap().push(format!("{host}:{port}"));
                let mut t = tokio::net::TcpStream::connect((host.as_str(), port))
                    .await
                    .unwrap();
                c.write_all(&[5, 0, 0, 1, 0, 0, 0, 0, 0, 0]).await.unwrap();
                let _ = tokio::io::copy_bidirectional(&mut c, &mut t).await;
            });
        }
    });
    (addr, seen)
}

#[tokio::test]
async fn socks5_proxy_is_used_for_the_connection() {
    let f = fake(Behavior::default()).await;
    let (proxy, seen) = socks_server().await;
    let mut ep = f.endpoint();
    ep.proxy = Socks5::parse(&format!("socks5://{proxy}"));
    let mut c = ProviderConfig::new(ep);
    c.connect_backoff = Duration::from_millis(10);
    ProviderPool::start(c, None).post(article(1)).await.unwrap();
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [format!("127.0.0.1:{}", f.addr.port())]
    );
    assert_eq!(f.posts().len(), 1);
}

#[tokio::test]
async fn client_round_robins_posts_and_combines_stat_answers() {
    let a = fake(Behavior {
        present: |_| false,
        ..Default::default()
    })
    .await;
    let b = fake(Behavior {
        present: |id| id == "on-b@x",
        ..Default::default()
    })
    .await;
    let client = Client::from_configs(vec![cfg(&a), cfg(&b)], 0);
    for i in 0..10 {
        client.post(article(i)).await.unwrap();
    }
    assert_eq!((a.posts().len(), b.posts().len()), (5, 5));
    assert!(
        client.stat("on-b@x").await.unwrap(),
        "found on the second provider"
    );
    assert!(
        !client.stat("nowhere@x").await.unwrap(),
        "missing everywhere"
    );

    let broken = fake(Behavior {
        stat_code: Some(500),
        ..Default::default()
    })
    .await;
    let missing = fake(Behavior {
        present: |_| false,
        ..Default::default()
    })
    .await;
    let client = Client::from_configs(vec![cfg(&broken), cfg(&missing)], 0);
    assert!(
        client.stat("x@x").await.is_err(),
        "an erroring provider must not turn into 'missing'"
    );
    assert_eq!(client.stats().len(), 2);
}

#[tokio::test]
async fn close_stops_new_work() {
    let f = fake(Behavior::default()).await;
    let pool = ProviderPool::start(cfg(&f), None);
    pool.post(article(1)).await.unwrap();
    pool.close();
    assert_eq!(pool.post(article(2)).await, Err(NntpError::Closed));
    assert_eq!(
        Client::new(vec![]).post(article(3)).await,
        Err(NntpError::Closed)
    );
}

#[tokio::test]
async fn providers_can_be_added_and_removed_while_running() {
    let a = fake(Behavior::default()).await;
    let b = fake(Behavior::default()).await;
    let client = Client::from_configs(vec![cfg(&a)], 0);
    client.post(article(0)).await.unwrap();
    assert_eq!((a.posts().len(), b.posts().len()), (1, 0));
    client.add_provider(cfg(&b));
    assert_eq!(client.stats().len(), 2);
    assert_eq!(client.max_connections(), 4);
    for i in 0..6 {
        client.post(article(i)).await.unwrap();
    }
    assert!(!b.posts().is_empty(), "the new provider takes its share");
    let key = a.endpoint().key();
    assert!(client.remove_provider(&key));
    assert!(!client.remove_provider(&key), "already gone");
    let before = a.posts().len();
    for i in 0..4 {
        client.post(article(i)).await.unwrap();
    }
    assert_eq!(
        a.posts().len(),
        before,
        "a removed provider gets no more work"
    );
    assert!(client.remove_provider(&b.endpoint().key()));
    assert!(client.post(article(9)).await.is_err(), "no providers left");
}

#[tokio::test]
async fn test_provider_reports_success_and_the_failure_kinds() {
    let ok = fake(Behavior::default()).await;
    assert!(postie_nntp::test_provider(&ok.endpoint()).await.is_ok());
    let mut dead = Endpoint::new("127.0.0.1", 1);
    dead.connect_timeout = Duration::from_millis(500);
    assert!(matches!(
        postie_nntp::test_provider(&dead).await,
        Err(NntpError::Connect(_) | NntpError::Timeout)
    ));
}
