// Connection ramp-up must be parallel even when the server greets slowly.
use std::time::{Duration, Instant};

use postie_nntp::Endpoint;
use postie_nntp::pool::{ProviderConfig, ProviderPool};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

async fn slow_server() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let (s, _) = l.accept().await.unwrap();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(300)).await;
                let (r, mut w) = s.into_split();
                let mut r = BufReader::new(r);
                w.write_all(b"200 hi\r\n").await.unwrap();
                let mut line = String::new();
                loop {
                    line.clear();
                    if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    if line.starts_with("POST") {
                        w.write_all(b"340 go\r\n").await.unwrap();
                        loop {
                            line.clear();
                            if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                                return;
                            }
                            if line == ".\r\n" {
                                break;
                            }
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        w.write_all(b"240 ok\r\n").await.unwrap();
                    } else if line.starts_with("STAT") {
                        w.write_all(b"223 0 <x>\r\n").await.unwrap();
                    }
                }
            });
        }
    });
    port
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ramp_up_dials_in_parallel() {
    let port = slow_server().await;
    let mut ep = Endpoint::new("127.0.0.1", port);
    ep.tls = false;
    let mut c = ProviderConfig::new(ep);
    c.connections = 8;
    c.inflight = 10;
    let pool = ProviderPool::start(c, None);
    let t = Instant::now();
    let hs: Vec<_> = (0..200)
        .map(|i| {
            let p = pool.clone();
            tokio::spawn(async move {
                p.post(format!("Subject: {i}\r\n\r\nbody\r\n").into_bytes())
                    .await
            })
        })
        .collect();
    tokio::time::sleep_until(tokio::time::Instant::from_std(
        t + Duration::from_millis(1500),
    ))
    .await;
    let connected = pool.stats().connects;
    for h in hs {
        h.await.unwrap().unwrap();
    }
    assert!(
        connected >= 8,
        "expected all 8 connections within 1.5 s of a 300 ms greeting (serial ramp-up needs >= 2.4 s), got {connected}"
    );
}
