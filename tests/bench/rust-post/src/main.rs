//! Prototype uploader: yEnc + POST, 20 conns, TLS optional, mirroring nntppool's two-phase
//! POST (send `POST`, wait for 340, then body; up to `--depth` replies outstanding).
mod yenc;

use std::os::unix::fs::FileExt;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Semaphore};

const LINE: usize = 128;

struct Cfg {
    host: String,
    port: u16,
    tls: bool,
    conns: usize,
    file: String,
    article: usize,
    depth: usize,
}

fn arg<T: std::str::FromStr>(a: &[String], k: &str, d: T) -> T {
    a.iter().position(|x| x == k).and_then(|i| a.get(i + 1)).and_then(|v| v.parse().ok()).unwrap_or(d)
}

#[derive(Debug)]
struct NoVerify(Arc<rustls::crypto::CryptoProvider>);
impl rustls::client::danger::ServerCertVerifier for NoVerify {
    fn verify_server_cert(
        &self,
        _: &rustls::pki_types::CertificateDer<'_>,
        _: &[rustls::pki_types::CertificateDer<'_>],
        _: &rustls::pki_types::ServerName<'_>,
        _: &[u8],
        _: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, m: &[u8], c: &rustls::pki_types::CertificateDer<'_>, d: &rustls::DigitallySignedStruct) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &rustls::pki_types::CertificateDer<'_>, d: &rustls::DigitallySignedStruct) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn encode_bench() {
    let src: Vec<u8> = (0..750_000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
    let mut out = Vec::with_capacity(800_000);
    let t = Instant::now();
    let mut n = 0u64;
    while t.elapsed().as_secs_f64() < 3.0 {
        out.clear();
        yenc::encode_into(&src, LINE, &mut out);
        n += src.len() as u64;
    }
    let s = t.elapsed().as_secs_f64();
    println!("yenc scalar single-core: {:.2} GB/s", n as f64 / s / 1e9);
}

async fn conn_task(cfg: Arc<Cfg>, file: Arc<std::fs::File>, size: u64, total: usize, next: Arc<AtomicUsize>, sent: Arc<AtomicU64>, tlsc: Option<Arc<rustls::ClientConfig>>) -> std::io::Result<()> {
    let tcp = TcpStream::connect((cfg.host.as_str(), cfg.port)).await?;
    tcp.set_nodelay(true)?;
    let mut stream: Box<dyn Stream> = match tlsc {
        Some(c) => {
            let name = rustls::pki_types::ServerName::try_from(cfg.host.clone()).unwrap();
            Box::new(tokio_rustls::TlsConnector::from(c).connect(name, tcp).await?)
        }
        None => Box::new(tcp),
    };
    let mut greet = [0u8; 256];
    let n = stream.read(&mut greet).await?;
    if !greet[..n].starts_with(b"200") {
        return Err(std::io::Error::other("bad greeting"));
    }
    let (rd, mut wr) = tokio::io::split(stream);
    let sem = Arc::new(Semaphore::new(cfg.depth));
    let (rtx, mut rrx) = mpsc::channel::<()>(1);
    let rsem = sem.clone();
    let reader = tokio::spawn(async move {
        let mut r = BufReader::with_capacity(8192, rd);
        let mut line = String::new();
        loop {
            line.clear();
            if r.read_line(&mut line).await? == 0 {
                return Ok::<(), std::io::Error>(());
            }
            if line.starts_with("340") {
                let _ = rtx.send(()).await;
            } else if line.starts_with("240") {
                rsem.add_permits(1);
            } else if line.starts_with("205") {
                return Ok(());
            } else {
                return Err(std::io::Error::other(format!("unexpected reply: {line}")));
            }
        }
    });
    let mut raw = vec![0u8; cfg.article];
    let mut buf: Vec<u8> = Vec::with_capacity(cfg.article + cfg.article / 20 + 1024);
    let name = cfg.file.rsplit('/').next().unwrap_or("file").to_string();
    loop {
        let idx = next.fetch_add(1, Ordering::Relaxed);
        if idx >= total {
            break;
        }
        let begin = (idx * cfg.article) as u64;
        let len = ((size - begin) as usize).min(cfg.article);
        file.read_exact_at(&mut raw[..len], begin)?;
        buf.clear();
        buf.extend_from_slice(
            format!(
                "From: bench <bench@example.com>\r\nNewsgroups: alt.binaries.test\r\nSubject: [{}/{}] \"{}\" yEnc ({}/{})\r\nMessage-ID: <{:016x}.{}@bench>\r\n\r\n=ybegin part={} total={} line={} size={} name={}\r\n=ypart begin={} end={}\r\n",
                idx + 1, total, name, idx + 1, total, (idx as u64).wrapping_mul(0x9E3779B97F4A7C15), idx, idx + 1, total, LINE, size, name, begin + 1, begin + len as u64
            )
            .as_bytes(),
        );
        yenc::encode_into(&raw[..len], LINE, &mut buf);
        buf.extend_from_slice(format!("=yend size={} part={} pcrc32={:08x}\r\n.\r\n", len, idx + 1, crc32fast::hash(&raw[..len])).as_bytes());
        sem.acquire().await.unwrap().forget();
        wr.write_all(b"POST\r\n").await?;
        wr.flush().await?;
        rrx.recv().await.ok_or_else(|| std::io::Error::other("reader closed before 340"))?;
        wr.write_all(&buf).await?;
        wr.flush().await?;
        sent.fetch_add(buf.len() as u64, Ordering::Relaxed);
    }
    let _ = sem.acquire_many(cfg.depth as u32).await.unwrap();
    wr.write_all(b"QUIT\r\n").await?;
    wr.flush().await?;
    reader.await.map_err(std::io::Error::other)??;
    Ok(())
}

trait Stream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send {}
impl<T: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send> Stream for T {}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let a: Vec<String> = std::env::args().collect();
    if a.iter().any(|x| x == "--encode-bench") {
        encode_bench();
        return Ok(());
    }
    let cfg = Arc::new(Cfg {
        host: arg(&a, "--host", "127.0.0.1".to_string()),
        port: arg(&a, "--port", 1199),
        tls: a.iter().any(|x| x == "--tls"),
        conns: arg(&a, "--conns", 20),
        file: arg(&a, "--file", String::new()),
        article: arg(&a, "--article", 750_000),
        depth: arg(&a, "--depth", 10),
    });
    let file = Arc::new(std::fs::File::open(&cfg.file)?);
    let size = file.metadata()?.len();
    let total = (size as usize).div_ceil(cfg.article);
    let tlsc = cfg.tls.then(|| {
        let p = Arc::new(rustls::crypto::ring::default_provider());
        let c = rustls::ClientConfig::builder_with_provider(p.clone())
            .with_safe_default_protocol_versions()
            .unwrap()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerify(p)))
            .with_no_client_auth();
        Arc::new(c)
    });
    let (next, sent) = (Arc::new(AtomicUsize::new(0)), Arc::new(AtomicU64::new(0)));
    let t = Instant::now();
    let mut hs = Vec::new();
    for _ in 0..cfg.conns {
        hs.push(tokio::spawn(conn_task(cfg.clone(), file.clone(), size, total, next.clone(), sent.clone(), tlsc.clone())));
    }
    for h in hs {
        h.await.map_err(std::io::Error::other)??;
    }
    let s = t.elapsed().as_secs_f64();
    println!("rust articles={} sent_bytes={} wall_ms={:.0} MiB/s={:.1} tls={} depth={} conns={}", total, sent.load(Ordering::Relaxed), s * 1000.0, size as f64 / 1048576.0 / s, cfg.tls, cfg.depth, cfg.conns);
    Ok(())
}
