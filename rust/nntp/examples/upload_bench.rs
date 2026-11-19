//! Uploads a file as yEnc articles through `postie-nntp` (same flags and output as the
//! `tests/bench/rust-post` prototype) so the real crate can be timed against the Go sink.
//! usage: upload_bench --file F [--host H] [--port P] [--tls] [--conns N] [--depth D] [--article B]

use std::os::unix::fs::FileExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

use postie_nntp::Endpoint;
use postie_nntp::pool::{Client, ProviderConfig};

fn arg<T: std::str::FromStr>(a: &[String], k: &str, d: T) -> T {
    a.iter()
        .position(|x| x == k)
        .and_then(|i| a.get(i + 1))
        .and_then(|v| v.parse().ok())
        .unwrap_or(d)
}

#[tokio::main]
async fn main() {
    let a: Vec<String> = std::env::args().collect();
    let file_path: String = arg(&a, "--file", String::new());
    let conns: usize = arg(&a, "--conns", 20);
    let depth: usize = arg(&a, "--depth", 10);
    let article: usize = arg(&a, "--article", 750_000);
    let tls = a.iter().any(|x| x == "--tls");
    let mut ep = Endpoint::new(
        &arg(&a, "--host", "127.0.0.1".to_string()),
        arg(&a, "--port", 1199),
    );
    ep.tls = tls;
    ep.insecure_tls = true;
    let mut cfg = ProviderConfig::new(ep);
    cfg.connections = conns;
    cfg.inflight = depth;
    let client = Arc::new(Client::from_configs(vec![cfg], 0));

    let file = Arc::new(std::fs::File::open(&file_path).expect("open file"));
    let size = file.metadata().unwrap().len();
    let total = (size as usize).div_ceil(article);
    let name = file_path.rsplit('/').next().unwrap_or("file").to_string();
    let next = Arc::new(AtomicUsize::new(0));
    let sent = Arc::new(AtomicU64::new(0));
    let t = Instant::now();
    let workers: Vec<_> = (0..conns * 3)
        .map(|_| {
            let (client, file, next, sent, name) = (client.clone(), file.clone(), next.clone(), sent.clone(), name.clone());
            tokio::spawn(async move {
                let mut raw = vec![0u8; article];
                loop {
                    let idx = next.fetch_add(1, Ordering::Relaxed);
                    if idx >= total {
                        return;
                    }
                    let begin = (idx * article) as u64;
                    let len = ((size - begin) as usize).min(article);
                    file.read_exact_at(&mut raw[..len], begin).expect("read");
                    let header = format!("From: bench <bench@example.com>\r\nNewsgroups: alt.binaries.test\r\nSubject: [{}/{}] \"{}\" yEnc ({}/{})\r\nMessage-ID: <{:016x}.{}@bench>\r\n", idx + 1, total, name, idx + 1, total, (idx as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15), idx);
                    let wire = postie_yenc::encode_wire(&header, &name, size as i64, idx as i64 + 1, total as i64, begin as i64, len as i64, &raw[..len]).expect("encode");
                    sent.fetch_add(wire.len() as u64, Ordering::Relaxed);
                    client.post(wire).await.expect("post");
                }
            })
        })
        .collect();
    for w in workers {
        w.await.unwrap();
    }
    let s = t.elapsed().as_secs_f64();
    println!(
        "nntp-crate articles={} sent_bytes={} wall_ms={:.0} MiB/s={:.1} tls={} depth={} conns={}",
        total,
        sent.load(Ordering::Relaxed),
        s * 1000.0,
        size as f64 / 1_048_576.0 / s,
        tls,
        depth,
        conns
    );
}
