use md5::{Digest, Md5};
use std::time::Instant;
fn main() {
    let data = vec![0x5au8; 256 << 20];
    let t = Instant::now();
    let mut h = Md5::new();
    for _ in 0..4 {
        h.update(&data);
    }
    let _ = h.finalize();
    println!("md-5 crate: {:.0} MB/s", 1024.0 / t.elapsed().as_secs_f64());
    let t = Instant::now();
    for _ in 0..4 {
        let _ = crc32fast::hash(&data);
    }
    println!("crc32fast: {:.0} MB/s", 1024.0 / t.elapsed().as_secs_f64());
}
