//! usage: rust-par2 FILE OUT_PAR2 [SLICE_BYTES=10485760] [PERCENT=5]
use par2_rs::create::{BlockSizing, ForwardKernel, Par2Creator, Par2CreatorOptions, RecoveryAmount};
use std::path::PathBuf;
use std::time::Instant;

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let file = PathBuf::from(&a[1]);
    let out = PathBuf::from(&a[2]);
    let slice: u64 = a.get(3).and_then(|v| v.parse().ok()).unwrap_or(10 * 1024 * 1024);
    let pct: u32 = a.get(4).and_then(|v| v.parse().ok()).unwrap_or(5);
    let base = file.parent().map(PathBuf::from);
    let mut o = Par2CreatorOptions::with_output(out, base, vec![file]);
    o.block_sizing = BlockSizing::Bytes(slice);
    o.recovery_amount = RecoveryAmount::Percent(pct);
    o.forward_kernel = ForwardKernel::Auto;
    o.overwrite = true;
    if let Ok(k) = std::env::var("KERNEL") {
        o.forward_kernel = match k.as_str() { "simd" => ForwardKernel::Simd, "portable" => ForwardKernel::Portable, _ => ForwardKernel::Auto };
    }
    if let Ok(m) = std::env::var("MEM_MB") {
        o.memory_limit = m.parse::<usize>().ok().map(|v| v * 1024 * 1024);
    }
    let t = Instant::now();
    if std::env::var("PHASES").is_ok() {
        use std::sync::{Arc, Mutex};
        let last: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        o.progress = Some(Arc::new(move |u: par2_rs::types::ProgressUpdate| {
            let ph = format!("{:?}", u.phase);
            let mut l = last.lock().unwrap();
            if l.as_deref() != Some(ph.as_str()) {
                eprintln!("{:6.0} ms  phase {}", t.elapsed().as_secs_f64() * 1000.0, ph);
                *l = Some(ph);
            }
        }));
    }
    let c = Par2Creator::new(o);
    let plan = c.plan().expect("plan");
    let tp = t.elapsed();
    let outcome = c.create(&plan).expect("create");
    println!(
        "par2-rs plan_ms={:.0} total_ms={:.0} source_slices={} recovery_slices={} files={}",
        tp.as_secs_f64() * 1000.0,
        t.elapsed().as_secs_f64() * 1000.0,
        outcome.source_slice_count,
        outcome.recovery_count,
        outcome.output_paths.len()
    );
}
