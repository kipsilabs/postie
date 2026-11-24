//! usage: par2_bench FILE OUT_PAR2 [SLICE_BYTES=10485760] [PERCENT=5]
use postie_core::par2::create::{InputFile, Options, create_with_names};
use std::path::PathBuf;
use std::time::Instant;
use tokio_util::sync::CancellationToken;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let file = PathBuf::from(&a[1]);
    let out = PathBuf::from(&a[2]);
    let slice: usize = a
        .get(3)
        .and_then(|v| v.parse().ok())
        .unwrap_or(10 * 1024 * 1024);
    let pct: f64 = a.get(4).and_then(|v| v.parse().ok()).unwrap_or(5.0);
    let size = std::fs::metadata(&file).unwrap().len();
    let slices = size.div_ceil(slice as u64) as f64;
    let rec = ((slices * pct / 100.0).ceil() as usize).max(1);
    let o = Options {
        slice_size: slice,
        num_recovery: rec,
        num_threads: 0,
        memory_limit: 0,
        creator: "Postie".into(),
    };
    let t = Instant::now();
    create_with_names(
        &CancellationToken::new(),
        &out,
        &[InputFile {
            path: file,
            name: String::new(),
        }],
        &o,
        None,
    )
    .unwrap();
    println!(
        "rust-par2 total_ms={:.0} source_slices={slices} recovery_slices={rec}",
        t.elapsed().as_secs_f64() * 1000.0
    );
}
