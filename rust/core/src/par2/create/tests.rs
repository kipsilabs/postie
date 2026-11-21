use std::collections::BTreeMap;
use std::process::Command;
use std::sync::Mutex;

use super::*;

/// The deterministic byte stream the Go oracle used (xorshift32, one byte per step).
fn xs(seed: u32, n: usize) -> Vec<u8> {
    let mut s = seed;
    (0..n)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            (s & 0xff) as u8
        })
        .collect()
}

fn tmp() -> PathBuf {
    let p = std::env::temp_dir().join(format!(
        "par2-{}-{}",
        std::process::id(),
        rand::random::<u64>()
    ));
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn opts(slice: usize, rec: usize) -> Options {
    Options {
        slice_size: slice,
        num_recovery: rec,
        num_threads: 0,
        memory_limit: 0,
        creator: "Postie".into(),
    }
}

fn write_inputs(dir: &Path, files: &[(&str, u32, usize)]) -> Vec<InputFile> {
    files
        .iter()
        .map(|(name, seed, size)| {
            let p = dir.join(Path::new(name).file_name().unwrap());
            std::fs::write(&p, xs(*seed, *size)).unwrap();
            InputFile {
                path: p,
                name: (*name).into(),
            }
        })
        .collect()
}

fn set_files(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    std::fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".par2"))
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap(),
            )
        })
        .collect()
}

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/par2")
        .join(name)
}

fn have_par2() -> bool {
    Command::new("par2").arg("--version").output().is_ok()
}

/// Single-file sets are byte-identical to what Go's par2go produced for the same inputs and options.
#[test]
fn single_file_sets_are_byte_identical_to_par2go() {
    for (case, file, seed, size, slice, rec) in [
        ("single-small", "a.bin", 1, 5000, 4096, 2),
        ("single-100k", "b.bin", 2, 100_000, 16384, 3),
        ("single-exact", "c.bin", 5, 12288, 4096, 1),
        ("tiny", "t.bin", 6, 1, 4, 1),
    ] {
        let dir = tmp();
        let inputs = write_inputs(&dir, &[(file, seed, size)]);
        let out = dir.join("set.par2");
        create_with_names(
            &CancellationToken::new(),
            &out,
            &inputs,
            &opts(slice, rec),
            None,
        )
        .unwrap();
        let (got, want) = (set_files(&dir), set_files(&fixture(case)));
        assert_eq!(
            got.keys().collect::<Vec<_>>(),
            want.keys().collect::<Vec<_>>(),
            "{case}: same file names"
        );
        for (name, bytes) in &want {
            assert!(got[name] == *bytes, "{case}: {name} differs from par2go");
        }
    }
}

#[test]
fn the_volume_layout_doubles_after_the_first_two() {
    assert_eq!(volume_groups(5), [(0, 1), (1, 1), (2, 2), (4, 1)]);
    assert_eq!(volume_groups(1), [(0, 1)]);
    assert_eq!(volume_groups(16), [(0, 1), (1, 1), (2, 2), (4, 4), (8, 8)]);
    assert_eq!(volume_groups(0), []);
}

/// What par2go's multi-file output cannot guarantee: recovery of several damaged blocks across a
/// multi-file set (source slices are numbered in file-ID order). Verified by par2cmdline.
#[test]
fn multi_file_sets_repair_with_par2cmdline() {
    if !have_par2() {
        return eprintln!("skipped: par2cmdline not installed");
    }
    for order in [[0usize, 1, 2], [2, 1, 0], [1, 2, 0]] {
        let dir = tmp();
        let all = [
            ("p.bin", 11u32, 20000usize),
            ("q.bin", 12, 9000),
            ("r.bin", 13, 4097),
        ];
        let inputs = write_inputs(&dir, &order.map(|i| all[i]));
        create_with_names(
            &CancellationToken::new(),
            &dir.join("set.par2"),
            &inputs,
            &opts(4096, 5),
            None,
        )
        .unwrap();
        assert!(
            Command::new("par2")
                .args(["v", "-q", "set.par2"])
                .current_dir(&dir)
                .output()
                .unwrap()
                .status
                .success(),
            "intact set verifies ({order:?})"
        );
        // damage two blocks of one file and two of another, forcing non-zero exponents
        for (name, offsets) in [("p.bin", [5000usize, 13000]), ("q.bin", [100, 5000])] {
            let mut d = std::fs::read(dir.join(name)).unwrap();
            offsets
                .iter()
                .for_each(|&o| d[o..o + 50].iter_mut().for_each(|b| *b ^= 0xff));
            std::fs::write(dir.join(name), d).unwrap();
        }
        let out = Command::new("par2")
            .args(["r", "-q", "set.par2"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "repair failed for order {order:?}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        for (name, seed, size) in all {
            assert_eq!(
                std::fs::read(dir.join(name)).unwrap(),
                xs(seed, size),
                "{name} restored ({order:?})"
            );
        }
    }
}

#[test]
fn output_does_not_depend_on_thread_count_or_memory_chunking() {
    let dir = tmp();
    let inputs = write_inputs(&dir, &[("a.bin", 21, 70_000), ("b.bin", 22, 33_333)]);
    let mut reference: Option<BTreeMap<String, Vec<u8>>> = None;
    for (threads, mem) in [(1usize, 0i64), (7, 0), (3, 4096 * 2), (4, 4096)] {
        let out_dir = tmp();
        let o = Options {
            num_threads: threads,
            memory_limit: mem,
            ..opts(4096, 7)
        };
        create_with_names(
            &CancellationToken::new(),
            &out_dir.join("set.par2"),
            &inputs,
            &o,
            None,
        )
        .unwrap();
        let got = set_files(&out_dir);
        match &reference {
            None => reference = Some(got),
            Some(r) => assert!(
                *r == got,
                "threads={threads} memory_limit={mem} changed the output"
            ),
        }
    }
}

#[test]
fn empty_files_are_described_but_have_no_slices_and_an_all_empty_set_fails() {
    let dir = tmp();
    let inputs = write_inputs(&dir, &[("empty.bin", 1, 0), ("data.bin", 2, 5000)]);
    create_with_names(
        &CancellationToken::new(),
        &dir.join("set.par2"),
        &inputs,
        &opts(4096, 1),
        None,
    )
    .unwrap();
    let main = std::fs::read(dir.join("set.par2")).unwrap();
    assert!(main.windows(9).any(|w| w == b"empty.bin"));
    let only_empty = write_inputs(&tmp(), &[("e.bin", 1, 0)]);
    let err = create_with_names(
        &CancellationToken::new(),
        &tmp().join("s.par2"),
        &only_empty,
        &opts(4096, 1),
        None,
    )
    .unwrap_err();
    assert_eq!(
        err,
        Par2Error::Invalid("par2go: no input slices (all files empty?)".into())
    );
}

#[test]
fn invalid_arguments_have_the_go_messages() {
    let dir = tmp();
    let ok = write_inputs(&dir, &[("a.bin", 1, 100)]);
    let c = CancellationToken::new();
    let run = |inputs: &[InputFile], o: &Options| {
        create_with_names(&c, &dir.join("s.par2"), inputs, o, None)
            .unwrap_err()
            .to_string()
    };
    assert_eq!(
        run(&ok, &opts(0, 1)),
        "par2go: SliceSize must be a positive multiple of 4, got 0"
    );
    assert_eq!(
        run(&ok, &opts(10, 1)),
        "par2go: SliceSize must be a positive multiple of 4, got 10"
    );
    assert_eq!(
        run(&ok, &opts(4, 0)),
        "par2go: NumRecovery must be positive, got 0"
    );
    assert_eq!(run(&[], &opts(4, 1)), "par2go: no input files");
    let named = |n: &str| {
        vec![InputFile {
            name: n.into(),
            ..ok[0].clone()
        }]
    };
    assert!(run(&named("a\\b"), &opts(4, 1)).contains("must not contain backslash"));
    assert!(run(&named("/abs"), &opts(4, 1)).contains("must be relative, not absolute"));
    assert!(run(&named("x/../y"), &opts(4, 1)).contains("must not contain '..' segment"));
    let missing = vec![InputFile {
        path: dir.join("nope"),
        name: String::new(),
    }];
    assert!(run(&missing, &opts(4, 1)).starts_with("par2go: scanning failed: open "));
}

#[test]
fn cancellation_stops_creation_and_leaves_no_set() {
    let dir = tmp();
    let inputs = write_inputs(&dir, &[("a.bin", 1, 300_000)]);
    let c = CancellationToken::new();
    c.cancel();
    let err =
        create_with_names(&c, &dir.join("set.par2"), &inputs, &opts(4096, 2), None).unwrap_err();
    assert_eq!(err, Par2Error::Cancelled);
    assert!(!dir.join("set.par2").exists());
    // cancelling mid-run (from the progress callback) also aborts
    let c2 = CancellationToken::new();
    let cb = |phase: &str, p: f64| {
        if phase == "hashing" && p > 0.0 && p < 1.0 {
            c2.cancel();
        }
    };
    let o = Options {
        memory_limit: 4096,
        ..opts(4096, 4)
    };
    let err = create_with_names(&c2, &dir.join("set2.par2"), &inputs, &o, Some(&cb)).unwrap_err();
    assert_eq!(err, Par2Error::Cancelled);
}

#[test]
fn progress_reports_phases_in_order_and_finishes_at_one() {
    let dir = tmp();
    let inputs = write_inputs(&dir, &[("a.bin", 1, 200_000)]);
    let seen = Mutex::new(Vec::<(String, f64)>::new());
    let cb = |p: &str, v: f64| seen.lock().unwrap().push((p.to_string(), v));
    create_with_names(
        &CancellationToken::new(),
        &dir.join("set.par2"),
        &inputs,
        &opts(4096, 3),
        Some(&cb),
    )
    .unwrap();
    let seen = seen.into_inner().unwrap();
    let phases: Vec<&str> = seen.iter().map(|(p, _)| p.as_str()).collect();
    let first_enc = phases.iter().position(|p| *p == "encoding").unwrap();
    let first_write = phases.iter().position(|p| *p == "writing").unwrap();
    assert!(phases[..first_enc].iter().all(|p| *p == "hashing") && first_enc < first_write);
    assert_eq!(seen.last().unwrap(), &("writing".to_string(), 1.0));
    let hashing: Vec<f64> = seen
        .iter()
        .filter(|(p, _)| p == "hashing")
        .map(|(_, v)| *v)
        .collect();
    assert!(hashing.windows(2).all(|w| w[0] <= w[1]) && *hashing.last().unwrap() == 1.0);
}

/// A folder with thousands of files must not spawn a thread per file (macOS caps a process at
/// 4096 threads, so that panicked and silently dropped PAR2 for big app bundles).
#[test]
fn thousands_of_input_files_do_not_need_a_thread_each() {
    let dir = tmp();
    let inputs: Vec<InputFile> = (0..5000u32)
        .map(|i| {
            let p = dir.join(format!("f{i}.bin"));
            std::fs::write(&p, xs(i + 1, 16)).unwrap();
            InputFile {
                path: p,
                name: String::new(),
            }
        })
        .collect();
    let c = CancellationToken::new();
    create_with_names(&c, &dir.join("many.par2"), &inputs, &opts(16, 1), None)
        .expect("5000 single-slice files fit in one set");
    assert!(dir.join("many.par2").exists());
}

/// PAR2 has 32768 distinct input-slice constants; more slices must be a clear error, not a panic.
#[test]
fn more_than_32768_input_slices_is_an_error_not_a_panic() {
    let dir = tmp();
    let inputs = write_inputs(&dir, &[("big.bin", 7, (MAX_INPUT_SLICES + 1) * 4)]);
    let c = CancellationToken::new();
    let err = create_with_names(&c, &dir.join("s.par2"), &inputs, &opts(4, 1), None)
        .expect_err("too many slices");
    assert!(
        err.to_string().contains("32768"),
        "message names the limit: {err}"
    );
    assert!(!dir.join("s.par2").exists());
}

#[test]
fn tiling_and_kernel_choice_do_not_change_the_output() {
    let dir = tmp();
    // the slice spans several tiles with a ragged last one, and odd worker counts split it unevenly
    let slice = 3 * 4096 + 68;
    let inputs = write_inputs(
        &dir,
        &[("a.bin", 31, 5 * slice + 1000), ("b.bin", 32, slice / 2)],
    );
    let mut reference: Option<BTreeMap<String, Vec<u8>>> = None;
    for (tile, batch_width, threads) in [
        (usize::MAX, 16, 1),
        (4096, 16, 3),
        (4100, 1, 3),
        (64, 16, 2),
        (TILE_BYTES, 1, 4),
        (TILE_BYTES, 16, 1),
    ] {
        let out_dir = tmp();
        let o = Options {
            num_threads: threads,
            ..opts(slice, 5)
        };
        create_tuned(
            &CancellationToken::new(),
            &out_dir.join("set.par2"),
            &inputs,
            &o,
            None,
            Tuning { tile, batch_width },
        )
        .unwrap();
        let got = set_files(&out_dir);
        match &reference {
            None => reference = Some(got),
            Some(r) => assert!(
                *r == got,
                "tile={tile} batch_width={batch_width} threads={threads} changed the output"
            ),
        }
    }
}

#[test]
fn default_workers_leave_a_core_for_the_md5_and_reader_threads() {
    assert_eq!(worker_threads(0, 10), 9);
    assert_eq!(worker_threads(0, 2), 1);
    assert_eq!(worker_threads(0, 1), 1);
    assert_eq!(worker_threads(4, 10), 4, "an explicit count is kept");
}
