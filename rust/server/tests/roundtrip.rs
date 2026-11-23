//! Release-stability suite: every upload here is downloaded back through an independent NZB/yEnc
//! reader and compared byte for byte with the originals, and its PAR2 set is checked (and used for a
//! repair) by par2cmdline. Faults are injected deterministically: lost articles, rejected posts,
//! dropped connections and an app restart mid-upload.

mod support;

use std::path::{Path, PathBuf};

use serde_json::Value;
use support::download::{self, Decoded};
use support::*;

/// The article size of `fast_config`.
const A: usize = 100_000;

fn config(
    dir: &Path,
    fake: &Fake,
    obfuscation: &str,
    par2: bool,
) -> postie_core::config::ConfigData {
    let mut c = fast_config(dir, fake.addr.port());
    c.posting.obfuscation_policy = obfuscation.into();
    c.posting.par2_obfuscation_policy = obfuscation.into();
    c.par2.enabled = Some(par2);
    c.par2.redundancy = "10%".into();
    c.par2.temp_dir = dir.join("par2tmp").to_string_lossy().into_owned();
    c
}

/// Writes `files` under `dir/src` and returns their paths.
fn write_sources(dir: &Path, files: &[(String, Vec<u8>)]) -> Vec<PathBuf> {
    files
        .iter()
        .map(|(n, b)| {
            let p = dir.join("src").join(n);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(&p, b).unwrap();
            p
        })
        .collect()
}

/// Adds paths the way the desktop app does (in place, no copy).
fn add(web: &Web, paths: &[PathBuf]) {
    let paths: Vec<String> = paths
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    web.app.handle_dropped_files(&paths).expect("add files");
}

/// Waits until `n` queue items exist and all are complete, then until verification has settled
/// with nothing failed. Returns the items.
async fn complete_and_verified(web: &Web, n: usize, secs: u64) -> Vec<Value> {
    let items = wait(&format!("{n} items complete"), secs, || async {
        let items = web.queue_items().await;
        if items.iter().any(|i| i["status"] == "error") {
            panic!("an upload failed: {items:?}");
        }
        (items.len() == n && items.iter().all(|i| i["status"] == "complete")).then_some(items)
    })
    .await;
    wait("verification settled", secs, || async {
        let s = web.get("/api/queue/stats").await;
        assert_eq!(s["verificationFailed"], 0, "verification failed: {s}");
        (s["pendingVerification"] == 0).then_some(())
    })
    .await;
    items
}

/// Downloads every item's NZB back from the fake server.
fn download_all(items: &[Value], fake: &Fake) -> Vec<(String, Vec<Decoded>)> {
    items
        .iter()
        .map(|i| {
            let nzb = i["nzbPath"]
                .as_str()
                .unwrap_or_else(|| panic!("no NZB: {i}"));
            let xml = download::read_nzb(Path::new(nzb));
            (xml.clone(), download::download(&xml, fake))
        })
        .collect()
}

fn sized(names: &[(&str, usize)]) -> Vec<(String, Vec<u8>)> {
    names
        .iter()
        .enumerate()
        .map(|(i, (n, len))| {
            // distinct content per file, even for equal sizes
            let mut b = blob(*len + i);
            b.truncate(*len);
            (n.to_string(), b)
        })
        .collect()
}

/// Checks the PAR2 set of one download with par2cmdline: it must verify, and repair a damaged file.
fn par2_verify_and_repair(work: &Path, decoded: &[Decoded], originals: &[(String, Vec<u8>)]) {
    if !decoded.iter().any(Decoded::is_par2) {
        // Postie skips PAR2 for files too small for one SIMD-safe block
        let total: usize = originals.iter().map(|(_, b)| b.len()).sum();
        assert!(
            total < 4096,
            "no PAR2 set was posted for {total} bytes of data"
        );
        return;
    }
    let descs = download::par2_file_descs(
        &decoded
            .iter()
            .filter(|d| d.is_par2())
            .cloned()
            .collect::<Vec<_>>(),
    );
    for d in &descs {
        let base = Path::new(&d.name)
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            originals
                .iter()
                .any(|(n, _)| Path::new(n).file_name().unwrap() == base.as_str()),
            "PAR2 records {:?}, which is none of the original names",
            d.name
        );
    }
    let Some(bin) = download::par2_cmd() else {
        return;
    };
    let par2 = download::materialize(work, decoded);
    let (ok, out) = download::run_par2(bin, "verify", &par2);
    assert!(ok, "par2 verify failed:\n{out}");
    // damage the largest data file and let par2cmdline repair it from the recovery slices
    let big = descs.iter().max_by_key(|d| d.len).unwrap();
    if big.len == 0 {
        return;
    }
    let path = work.join(&big.name);
    let mut bytes = std::fs::read(&path).unwrap();
    let mid = bytes.len() / 2;
    let end = (mid + 64).min(bytes.len());
    for b in &mut bytes[mid..end] {
        *b ^= 0xA5;
    }
    std::fs::write(&path, &bytes).unwrap();
    let (ok, out) = download::run_par2(bin, "verify", &par2);
    assert!(!ok, "par2 verify missed a damaged file:\n{out}");
    let (ok, out) = download::run_par2(bin, "repair", &par2);
    assert!(ok, "par2 repair failed:\n{out}");
    assert_eq!(
        download::md5_of(&std::fs::read(&path).unwrap()),
        big.md5,
        "repair did not restore {}",
        big.name
    );
}

async fn round_trip(obfuscation: &str) {
    let fake = fake_nntp().await;
    let dir = temp_dir(&format!("rt-{obfuscation}"));
    let web = Web::start_with(&dir, config(&dir, &fake, obfuscation, true)).await;
    // sizes around the article boundary, where off-by-ones live; every stem has a space so the
    // leak check below cannot match random obfuscated text (a bare "two" did, now and then)
    let files = sized(&[
        ("one byte.bin", 1),
        ("just below.bin", A - 1),
        ("exactly one.bin", A),
        ("just above.bin", A + 1),
        ("two articles.bin", 2 * A),
        ("ñandú [日本].mkv", 1_234_567),
    ]);
    add(&web, &write_sources(&dir, &files));
    let items = complete_and_verified(&web, files.len(), 120).await;
    for (i, (xml, decoded)) in download_all(&items, &fake).into_iter().enumerate() {
        let item_name = items[i]["fileName"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let original: Vec<_> = files
            .iter()
            .filter(|(n, _)| item_name.contains(n.as_str()) || n.contains(&item_name))
            .cloned()
            .collect();
        assert_eq!(original.len(), 1, "item {item_name} matches one source");
        download::assert_same_files(&decoded, &original);
        // the NZB is the uploader's private key: it keeps the real name so downloaders can use it
        let stem = Path::new(&original[0].0)
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(xml.contains(&stem), "the NZB lost the real name {stem}");
        par2_verify_and_repair(&dir.join(format!("dl-{i}")), &decoded, &original);
    }
    if obfuscation == "full" {
        for id in fake.article_ids() {
            let raw = fake.article(&id).unwrap();
            let sep = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
            let head = String::from_utf8_lossy(&raw[..sep + 4]).into_owned();
            let body_start =
                String::from_utf8_lossy(&raw[sep + 4..(sep + 200).min(raw.len())]).into_owned();
            for (n, _) in &files {
                let stem = Path::new(n)
                    .file_stem()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                assert!(
                    !head.contains(&stem),
                    "full obfuscation leaks {n} in headers:\n{head}"
                );
                assert!(
                    !body_start.contains(&stem),
                    "full obfuscation leaks {n} in =ybegin"
                );
            }
        }
    }
    web.stop().await;
}

#[tokio::test]
async fn uploads_without_obfuscation_download_back_identical_and_par2_repairs_them() {
    round_trip("none").await;
}

#[tokio::test]
async fn uploads_with_partial_obfuscation_download_back_identical_and_par2_repairs_them() {
    round_trip("partial").await;
}

#[tokio::test]
async fn uploads_with_full_obfuscation_download_back_identical_and_leak_no_names() {
    round_trip("full").await;
}

#[tokio::test]
async fn articles_lost_after_the_server_accepted_them_are_reposted_and_the_download_is_whole() {
    let fake = fake_nntp().await;
    fake.inject(&[2, 5, 9], &[], &[]);
    let dir = temp_dir("rt-lost");
    let web = Web::start_with(&dir, config(&dir, &fake, "none", false)).await;
    let files = sized(&[("lost.bin", 10 * A + 17)]);
    add(&web, &write_sources(&dir, &files));
    let items = complete_and_verified(&web, 1, 120).await;
    let (_, decoded) = download_all(&items, &fake).remove(0);
    download::assert_same_files(&decoded, &files);
    assert!(
        fake.posts() >= 11 + 3,
        "the 3 lost articles were posted again: {}",
        fake.posts()
    );
    web.stop().await;
}

#[tokio::test]
async fn a_rejected_post_and_a_dropped_connection_are_retried_and_the_download_is_whole() {
    let fake = fake_nntp().await;
    fake.inject(&[], &[3], &[6]);
    let dir = temp_dir("rt-flaky");
    let web = Web::start_with(&dir, config(&dir, &fake, "none", false)).await;
    let files = sized(&[("flaky.bin", 10 * A + 5)]);
    add(&web, &write_sources(&dir, &files));
    let items = complete_and_verified(&web, 1, 120).await;
    let (_, decoded) = download_all(&items, &fake).remove(0);
    download::assert_same_files(&decoded, &files);
    web.stop().await;
}

#[tokio::test]
async fn an_upload_stopped_halfway_resumes_after_a_restart_without_posting_everything_again() {
    let fake = fake_nntp().await;
    fake.set_post(PostMode::Delay(150));
    let dir = temp_dir("rt-resume");
    let web = Web::start_with(&dir, config(&dir, &fake, "none", false)).await;
    let articles = 30;
    let files = sized(&[("resume.bin", articles * A)]);
    add(&web, &write_sources(&dir, &files));
    wait("a third of the articles posted", 60, || async {
        (fake.article_ids().len() >= articles / 3).then_some(())
    })
    .await;
    web.stop().await;
    let before = fake.posts();
    fake.set_post(PostMode::Accept);
    let web = Web::restart(&dir).await;
    let items = complete_and_verified(&web, 1, 120).await;
    let reposted = fake.posts() - before;
    assert!(
        reposted < articles,
        "the restart posted {reposted} articles: it started over instead of resuming"
    );
    let (_, decoded) = download_all(&items, &fake).remove(0);
    download::assert_same_files(&decoded, &files);
    web.stop().await;
}

#[tokio::test]
async fn a_folder_of_a_thousand_small_files_uploads_with_par2_and_downloads_back_identical() {
    let fake = fake_nntp().await;
    let dir = temp_dir("rt-folder");
    let mut cfg = config(&dir, &fake, "none", true);
    cfg.servers.as_mut().unwrap()[0].max_connections = 8;
    let web = Web::start_with(&dir, cfg).await;
    let files: Vec<(String, Vec<u8>)> = (0..1000)
        .map(|i| {
            let mut b = blob(1000 + i * 7);
            b.rotate_left(i % 97);
            (format!("bundle/d{}/f{i:04}.dat", i % 10), b)
        })
        .collect();
    write_sources(&dir, &files);
    add(&web, &[dir.join("src/bundle")]);
    let items = complete_and_verified(&web, 1, 300).await;
    let (_, decoded) = download_all(&items, &fake).remove(0);
    download::assert_same_files(&decoded, &files);
    par2_verify_and_repair(&dir.join("dl"), &decoded, &files);
    web.stop().await;
}
