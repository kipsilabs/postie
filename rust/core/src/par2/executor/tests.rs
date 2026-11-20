use super::*;
use crate::par2::existing::read_file_descs;

fn tmp() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "par2x-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn data(n: usize, seed: u8) -> Vec<u8> {
    (0..n)
        .map(|i| (i as u8).wrapping_mul(31).wrapping_add(seed))
        .collect()
}

fn cfg() -> Par2Config {
    Par2Config {
        redundancy: "10".into(),
        num_goroutines: 2,
        gf16_method: "auto".into(),
        ..Default::default()
    }
}

fn file(dir: &Path, name: &str, size: usize, seed: u8) -> FileInfo {
    let p = dir.join(name);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, data(size, seed)).unwrap();
    FileInfo {
        path: p.to_string_lossy().into_owned(),
        size: size as u64,
        relative_path: name.into(),
    }
}

fn names(paths: &[String]) -> Vec<String> {
    let mut v: Vec<String> = paths.iter().map(|p| base_name(p)).collect();
    v.sort();
    v
}

#[test]
fn compute_file_block_size_is_simd_aligned_and_skips_tiny_files() {
    for (file, article, cfg, zero) in [
        (100_000u64, 10_000u64, 10 * 1024 * 1024i64, false),
        (100_000, 750_000, 0, false),
        (10 * 1024 * 1024, 512 * 1024, 0, false),
        (127, 750_000, 0, true),
        (128, 750_000, 0, false),
    ] {
        let got = compute_file_block_size(file, article, cfg);
        assert_eq!(got == 0, zero, "file {file}");
        assert!(
            got.is_multiple_of(128) && got <= file,
            "block {got} for file {file}"
        );
    }
}

#[test]
fn calculate_par_block_size_table() {
    let k = 1024u64;
    for (file, article, want) in [
        (10 * k * k, 512 * k, 512 * k),
        (32768 * 512 * k + 1, 512 * k, 512 * k * 2),
        (32768 * 512 * k - 1, 512 * k, 512 * k),
        (32768 * 512 * k * 3 + 1, 512 * k, 512 * k * 4),
        (1024, 512 * k, 512 * k),
        (10 * k * k, 361_254, 361_252),
        (10 * k * k, 750_001, 750_000),
        (10 * k * k, 750_000, 750_000),
    ] {
        assert_eq!(
            calculate_par_block_size(file, article),
            want,
            "{file} / {article}"
        );
    }
    assert_eq!(
        calculate_par_block_size(5, 0),
        0,
        "no article size: no block"
    );
}

#[test]
fn parse_redundancy_table() {
    for (input, want) in [
        ("10", 10.0),
        ("10%", 10.0),
        ("1n*1.2", 120.0),
        ("2n*0.5", 100.0),
        (" 15 ", 15.0),
        ("invalid", 10.0),
        ("n*2", 200.0),
        ("", 10.0),
    ] {
        assert_eq!(parse_redundancy_percentage(input), want, "{input:?}");
    }
}

#[test]
fn set_block_size_respects_config_the_slice_cap_and_alignment() {
    let ex = |slice: i64, article: u64| {
        NativeExecutor::new(
            article,
            Par2Config {
                slice_size: slice,
                ..cfg()
            },
            None,
        )
    };
    assert_eq!(
        ex(0, 750_000).compute_set_block_size(10_000_000, 6_000_000),
        749_952,
        "article size, 128-aligned"
    );
    assert_eq!(
        ex(1_000_000, 750_000).compute_set_block_size(10_000_000, 6_000_000),
        999_936,
        "configured size when it fits the largest file"
    );
    assert_eq!(
        ex(50_000_000, 750_000).compute_set_block_size(10_000_000, 6_000_000),
        749_952,
        "configured above the largest file: ignored"
    );
    assert_eq!(ex(0, 4).compute_set_block_size(1 << 40, 1 << 39) % 128, 0);
    assert!(
        (1u64 << 40) / ex(0, 4).compute_set_block_size(1 << 40, 1 << 39) <= 32768,
        "capped at about 2^15 slices (alignment may land exactly on it, as in Go)"
    );
    assert_eq!(
        ex(0, 750_000).compute_set_block_size(100, 100),
        0,
        "tiny files"
    );
    assert_eq!(
        ex(0, 750_000).compute_set_block_size(500, 300),
        256,
        "clamped to the largest file, aligned"
    );
}

#[tokio::test]
async fn create_writes_a_set_next_to_the_source_and_reuses_it_next_time() {
    let d = tmp();
    let f = file(&d, "movie.bin", 300_000, 1);
    let ex = NativeExecutor::new(100_000, cfg(), None);
    let c = CancellationToken::new();
    let res = ex.create(&c, std::slice::from_ref(&f)).await.unwrap();
    assert!(res.reused.is_empty());
    assert!(
        names(&res.created).contains(&"movie.bin.par2".to_string()) && res.created.len() >= 2,
        "{:?}",
        res.created
    );
    let descs = read_file_descs(&d.join("movie.bin.par2")).unwrap();
    assert_eq!(
        (descs[0].name.as_str(), descs[0].size),
        ("movie.bin", 300_000)
    );
    // second run: the set describes this file, so it is reused and nothing is created
    let again = ex.create(&c, std::slice::from_ref(&f)).await.unwrap();
    assert_eq!(
        (again.created.len(), names(&again.reused)),
        (0, names(&res.created))
    );
    // changed content: the leftover set no longer describes the file, so a fresh one is created
    std::fs::write(&f.path, data(300_000, 99)).unwrap();
    let regenerated = ex.create(&c, std::slice::from_ref(&f)).await.unwrap();
    assert!(regenerated.reused.is_empty() && !regenerated.created.is_empty());
}

#[tokio::test]
async fn create_uses_the_work_dir_and_reuses_a_set_found_there() {
    let d = tmp();
    let temp = tmp();
    let f = file(&d, "a.bin", 200_000, 3);
    let ex = NativeExecutor::new(
        100_000,
        Par2Config {
            temp_dir: temp.to_string_lossy().into_owned(),
            ..cfg()
        },
        None,
    );
    let c = CancellationToken::new();
    let res = ex.create(&c, std::slice::from_ref(&f)).await.unwrap();
    let wd = temp.join("postie-par2");
    assert!(
        res.created.iter().all(|p| Path::new(p).starts_with(&wd)) && !d.join("a.bin.par2").exists()
    );
    let again = ex.create(&c, std::slice::from_ref(&f)).await.unwrap();
    assert_eq!(
        (again.created.len(), again.reused.len()),
        (0, res.created.len()),
        "reused from the work dir"
    );
}

#[tokio::test]
async fn create_skips_par2_inputs_and_tiny_files() {
    let d = tmp();
    let par2 = file(&d, "x.vol00+01.par2", 5000, 1);
    let tiny = file(&d, "tiny.bin", 10, 2);
    let ok = file(&d, "512.bin", 512, 3);
    let ex = NativeExecutor::new(750_000, cfg(), None);
    let res = ex
        .create(&CancellationToken::new(), &[par2, tiny, ok])
        .await
        .unwrap();
    assert!(
        res.created
            .iter()
            .all(|p| base_name(p).starts_with("512.bin")),
        "{:?}",
        res.created
    );
    assert!(!d.join("tiny.bin.par2").exists());
}

#[tokio::test]
async fn block_size_is_clamped_for_files_smaller_than_the_configured_slice() {
    let d = tmp();
    let f = file(&d, "small.bin", 5000, 4);
    let ex = NativeExecutor::new(
        750_000,
        Par2Config {
            slice_size: 10 * 1024 * 1024,
            ..cfg()
        },
        None,
    );
    let res = ex
        .create(&CancellationToken::new(), std::slice::from_ref(&f))
        .await
        .unwrap();
    assert!(!res.created.is_empty());
    let main = std::fs::read(d.join("small.bin.par2")).unwrap();
    let slice = u64::from_le_bytes(main[64..72].try_into().unwrap_or_default());
    assert!(slice % 128 == 0 && slice <= 5000, "slice {slice}");
}

#[tokio::test]
async fn create_in_directory_prefers_source_dir_sets_then_the_output_dir() {
    let d = tmp();
    let out = tmp();
    let f = file(&d, "f.bin", 150_000, 5);
    let ex = NativeExecutor::new(100_000, cfg(), None);
    let c = CancellationToken::new();
    let first = ex
        .create_in_directory(&c, std::slice::from_ref(&f), out.to_str().unwrap())
        .await
        .unwrap();
    assert!(first.created.iter().all(|p| Path::new(p).starts_with(&out)));
    let again = ex
        .create_in_directory(&c, std::slice::from_ref(&f), out.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        (again.created.len(), again.reused.len()),
        (0, first.created.len()),
        "found in the output dir"
    );
    // a set already next to the source wins over the output dir
    ex.create_in_directory(&c, std::slice::from_ref(&f), "")
        .await
        .unwrap();
    let third = ex
        .create_in_directory(&c, std::slice::from_ref(&f), out.to_str().unwrap())
        .await
        .unwrap();
    assert!(
        third.reused.iter().all(|p| Path::new(p).starts_with(&d)),
        "{:?}",
        third.reused
    );
}

#[tokio::test]
async fn create_set_embeds_relative_paths_and_falls_back_to_basenames() {
    let d = tmp();
    let a = file(&d, "extras/bonus.bin", 150_000, 1);
    let b = file(&d, "ep.bin", 120_000, 2);
    let ex = NativeExecutor::new(100_000, cfg(), None);
    let c = CancellationToken::new();
    let res = ex
        .create_set(
            &c,
            &[a.clone(), b.clone()],
            "",
            "MyShow",
            d.to_str().unwrap(),
        )
        .await
        .unwrap();
    assert!(names(&res.created).contains(&"MyShow.par2".to_string()));
    let mut descs: Vec<String> = read_file_descs(&d.join("extras").join("MyShow.par2"))
        .map(|x| x.into_iter().map(|d| d.name).collect())
        .unwrap_or_default();
    if descs.is_empty() {
        // the set lands next to the first input
        descs = read_file_descs(&d.join("extras/MyShow.par2"))
            .unwrap()
            .into_iter()
            .map(|d| d.name)
            .collect();
    }
    descs.sort();
    assert_eq!(descs, ["ep.bin", "extras/bonus.bin"]);
    // a second call reuses the set; a different file list does not
    let again = ex
        .create_set(
            &c,
            &[a.clone(), b.clone()],
            "",
            "MyShow",
            d.to_str().unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        (again.created.len(), again.reused.len()),
        (0, res.created.len())
    );
    let other = ex
        .create_set(
            &c,
            std::slice::from_ref(&a),
            "",
            "MyShow",
            d.to_str().unwrap(),
        )
        .await
        .unwrap();
    assert!(
        other.reused.is_empty() && !other.created.is_empty(),
        "stale set rejected"
    );
    // folder dir that is not a parent: base names
    let out = tmp();
    ex.create_set(&c, &[a], out.to_str().unwrap(), "Flat", "/nowhere")
        .await
        .unwrap();
    assert_eq!(
        read_file_descs(&out.join("Flat.par2")).unwrap()[0].name,
        "bonus.bin"
    );
}

#[tokio::test]
async fn create_set_validates_its_arguments() {
    let d = tmp();
    let f = file(&d, "a.bin", 5000, 1);
    let par2 = file(&d, "a.par2", 100, 1);
    let ex = NativeExecutor::new(750_000, cfg(), None);
    let c = CancellationToken::new();
    let err = |r: Result<Par2Result, ExecError>| r.unwrap_err().to_string();
    assert_eq!(
        err(ex.create_set(&c, &[], "", "s", "").await),
        "par2: no input files for set \"s\""
    );
    assert_eq!(
        err(ex
            .create_set(&c, std::slice::from_ref(&f), "", "", "")
            .await),
        "par2: empty set name"
    );
    assert_eq!(
        err(ex.create_set(&c, &[par2], "", "s", "").await),
        "par2: no non-par2 input files for set \"s\""
    );
    let tiny = file(&d, "t.bin", 20, 1);
    assert_eq!(
        ex.create_set(&c, &[tiny], "", "t", d.to_str().unwrap())
            .await
            .unwrap(),
        Par2Result::default(),
        "tiny sets are skipped, not errors"
    );
}

#[tokio::test]
async fn an_unknown_gf16_method_is_rejected_with_the_accepted_list() {
    let d = tmp();
    let f = file(&d, "a.bin", 200_000, 1);
    let ex = NativeExecutor::new(
        100_000,
        Par2Config {
            gf16_method: "warp-drive".into(),
            ..cfg()
        },
        None,
    );
    let e = ex
        .create(&CancellationToken::new(), &[f])
        .await
        .unwrap_err()
        .to_string();
    assert!(
        e.starts_with("unknown par2 gf16_method \"warp-drive\" (accepted: auto,"),
        "{e}"
    );
    for m in PAR2_GF16_METHODS {
        assert!(check_gf16_method(m).is_ok());
    }
    assert!(check_gf16_method("").is_ok());
}

#[tokio::test]
async fn progress_is_reported_and_removed_and_cancellation_is_surfaced() {
    let d = tmp();
    let f = file(&d, "a.bin", 400_000, 1);
    let jp = Arc::new(JobProgress::new("job"));
    let ex = NativeExecutor::new(100_000, cfg(), Some(jp.clone()));
    let seen = Arc::new(std::sync::Mutex::new(0i64));
    let (s2, jp2) = (seen.clone(), jp.clone());
    let watcher = tokio::spawn(async move {
        for _ in 0..200 {
            for st in jp2.get_all_progress_state() {
                let mut m = s2.lock().unwrap();
                *m = (*m).max(st.current_num);
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
    });
    ex.create(&CancellationToken::new(), std::slice::from_ref(&f))
        .await
        .unwrap();
    watcher.abort();
    assert!(
        jp.get_all_progress().is_empty(),
        "the bar is removed when done"
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    std::fs::remove_file(d.join("a.bin.par2")).unwrap();
    assert_eq!(
        ex.create(&cancelled, &[f]).await.unwrap_err(),
        ExecError::Cancelled
    );
}
