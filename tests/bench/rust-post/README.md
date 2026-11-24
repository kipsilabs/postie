# rust-post: hot-path prototype (throwaway)

Question: is a Rust port of the upload hot path (yEnc + two-phase POST + TLS) meaningfully
cheaper than the current Go path against the same sink (`../nyuu/sink`, FAST=1)?

Pass/fail written BEFORE any timed run (2026-11-19):
- Compare at equal settings: 1 article = 750000 B, 20 conns, depth 10 (Postie's default
  inflight), TLS=1 as the main row, TLS=0 secondary. Median of >=5 interleaved runs,
  4 GiB payload so Go's startup (SQLite, NZB write) is a small share.
- PASS (port is worth it for speed): Rust CPU per MiB (user+sys) is >= 25% lower than Go over
  TLS AND throughput is no worse than -5%. Peak RSS is reported but not part of the verdict.
- FAIL: anything else; then the rewrite case rests on non-speed reasons.
- Equivalence: sink article count must equal Go's; sink bytes within the header-size delta.
- rapidyenc question: if scalar single-core encode (GB/s) is a small share of per-byte CPU,
  no FFI is needed.
- Crypto provider: ring (rustls 0.23).

## Results (2026-11-19, M-series 10 cores, 4096 MiB, 20 conns, depth 10, FAST=1 sink, medians of 5)
| mode | tool | wall ms | MiB/s | CPU ms/MiB | peak RSS MiB |
| --- | --- | ---: | ---: | ---: | ---: |
| TLS | rust (scalar yEnc, ring) | 1442 | 2841 | 2.07 | 35 |
| TLS | go (postie-cli) | 1449 | 2826 | 1.52 | 81 |
| plain | rust | 1340 | 3056 | 1.63 | 32 |
| plain | go | 1424 | 2877 | 1.23 | 70 |
Sink: 5727 articles both; bytes differ only by the header set (Rust 4432813288 vs Go 4433183796).
Scalar yEnc single core: 1.01 GB/s (~1 ms/MiB), i.e. most of Rust's extra CPU.
Verdict vs the threshold above: FAIL (CPU/MiB 36% higher over TLS, throughput +0.5%). Rust uses ~57% less RSS.
Caveats: loopback, Go sink shares the cores, both tools end near the same wall (~1.4 s), so throughput is not discriminating here; scalar encoder only, rapidyenc FFI not tested.

## Results with C rapidyenc over FFI (feature `rapidyenc`, 2026-11-19, same setup, medians of 5)
Encode microbench: 9.16 GB/s single core (scalar was 1.01 GB/s). Build: `RAPIDYENC_DIR=<checkout> cargo build --release --features rapidyenc --target-dir target-ry`; run compare with `RUST_POST_BIN=../rust-post/target-ry/release/rust-post`.
| mode | tool | wall ms | MiB/s | CPU ms/MiB | peak RSS MiB |
| --- | --- | ---: | ---: | ---: | ---: |
| TLS | rust+rapidyenc | 1322 | 3099 | 0.63 | 54 |
| TLS | go | 1460 | 2806 | 1.57 | 89 |
| plain | rust+rapidyenc | 1315 | 3114 | 0.45 | 53 |
| plain | go | 1448 | 2828 | 1.24 | 65 |
Verdict vs threshold: PASS (CPU/MiB -60% over TLS, throughput +10%). Sink articles 5727 both.
Caveats: prototype has no SQLite state, NZB writing, retries or verification; the Go CLI pays for those, so the real-app gap is smaller by an unmeasured amount. Loopback only. Go does no whole-file hashing here (duplicate lookup samples 3x64 KiB; obfuscation off).
