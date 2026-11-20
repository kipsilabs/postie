//! The `postie` binary against a fake NNTP server: exit codes, NZB output and error reporting.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use postie_core::config::{self, ServerConfig};

/// Accepts every POST (`reply` 240) or rejects it (`441`) and counts them.
fn fake_nntp(reply: &'static str) -> (SocketAddr, Arc<AtomicUsize>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let posts = Arc::new(AtomicUsize::new(0));
    let p2 = posts.clone();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let posts = p2.clone();
            std::thread::spawn(move || {
                let mut w = s.try_clone().unwrap();
                let mut r = BufReader::new(s);
                let _ = w.write_all(b"200 ready\r\n");
                let mut line = Vec::new();
                loop {
                    line.clear();
                    if r.read_until(b'\n', &mut line).unwrap_or(0) == 0 {
                        return;
                    }
                    let cmd = String::from_utf8_lossy(&line).to_uppercase();
                    let out: &[u8] = if cmd.starts_with("POST") {
                        let _ = w.write_all(b"340 go\r\n");
                        loop {
                            line.clear();
                            if r.read_until(b'\n', &mut line).unwrap_or(0) == 0 {
                                return;
                            }
                            if line == b".\r\n" {
                                break;
                            }
                        }
                        posts.fetch_add(1, Ordering::SeqCst);
                        reply.as_bytes()
                    } else if cmd.starts_with("STAT") {
                        b"223 0 x\r\n"
                    } else if cmd.starts_with("QUIT") {
                        let _ = w.write_all(b"205 bye\r\n");
                        return;
                    } else if cmd.starts_with("AUTHINFO USER") {
                        b"381 pass\r\n"
                    } else if cmd.starts_with("AUTHINFO PASS") {
                        b"281 ok\r\n"
                    } else {
                        b"500 unknown\r\n"
                    };
                    let _ = w.write_all(out);
                }
            });
        }
    });
    (addr, posts)
}

fn temp(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "postie-cli-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn write_config(dir: &Path, addr: SocketAddr) -> PathBuf {
    let mut c = config::default_config();
    c.servers = Some(vec![ServerConfig {
        host: "127.0.0.1".into(),
        port: i64::from(addr.port()),
        role: "upload".into(),
        max_connections: 2,
        ..Default::default()
    }]);
    c.posting.max_retries = 1;
    c.posting.retry_delay = config::Duration::new("100ms");
    c.posting.article_size_in_bytes = 100_000;
    c.par2.enabled = Some(false);
    c.post_check.enabled = Some(false);
    c.database.database_path = dir.join("postie.db").to_string_lossy().into_owned();
    c.output_dir = dir.join("out").to_string_lossy().into_owned();
    let p = dir.join("config.yaml");
    config::save_config(&c, &p).unwrap();
    p
}

/// The binary under test: the one cargo built, or `POSTIE_BIN` (the release job points it at the
/// packaged LTO binary, so what ships is what is tested).
fn run(args: &[&str]) -> Output {
    let bin = std::env::var("POSTIE_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_postie").into());
    Command::new(bin).args(args).output().unwrap()
}

fn file(dir: &Path, name: &str, len: usize) -> PathBuf {
    let mut x: u64 = 0x1234_5678_9ABC_DEF1;
    let data: Vec<u8> = (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            (x >> 24) as u8
        })
        .collect();
    let p = dir.join(name);
    std::fs::write(&p, data).unwrap();
    p
}

#[test]
fn a_one_shot_upload_posts_the_file_writes_the_nzb_and_exits_zero() {
    let (addr, posts) = fake_nntp("240 posted\r\n");
    let dir = temp("ok");
    let cfg = write_config(&dir, addr);
    let f = file(&dir, "movie.bin", 350_000);
    let out = run(&[
        "-c",
        cfg.to_str().unwrap(),
        "-d",
        dir.to_str().unwrap(),
        "-o",
        dir.join("nzb").to_str().unwrap(),
        "-i",
        f.to_str().unwrap(),
    ]);
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        posts.load(Ordering::SeqCst) >= 4,
        "350 kB / 100 kB articles"
    );
    let nzb: Vec<_> = std::fs::read_dir(dir.join("nzb"))
        .map(|d| d.flatten().collect())
        .unwrap_or_default();
    assert!(
        !nzb.is_empty(),
        "an NZB was written to the output directory"
    );
}

#[test]
fn rejected_posts_make_the_command_fail_with_a_non_zero_exit_and_a_message() {
    let (addr, _) = fake_nntp("441 posting failed\r\n");
    let dir = temp("reject");
    let cfg = write_config(&dir, addr);
    let f = file(&dir, "bad.bin", 150_000);
    let out = run(&[
        "-c",
        cfg.to_str().unwrap(),
        "-d",
        dir.to_str().unwrap(),
        "-i",
        f.to_str().unwrap(),
    ]);
    assert!(!out.status.success(), "must not report success");
    let text = String::from_utf8_lossy(&out.stderr).to_lowercase();
    assert!(text.contains("error") || text.contains("fail"), "{text}");
}

#[test]
fn a_missing_config_or_input_fails_fast_instead_of_hanging() {
    let dir = temp("missing");
    let t = std::time::Instant::now();
    let out = run(&["-c", dir.join("nope.yaml").to_str().unwrap()]);
    assert!(!out.status.success());
    let out = run(&["--definitely-not-a-flag"]);
    assert!(!out.status.success());
    let mut s = String::new();
    let _ = out.stderr.as_slice().read_to_string(&mut s);
    assert!(s.contains("unknown flag"), "{s}");
    assert!(t.elapsed() < std::time::Duration::from_secs(20));
}
