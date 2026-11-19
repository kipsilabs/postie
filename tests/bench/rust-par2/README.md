# rust-par2: par2-rs vs par2go PAR2 create (throwaway)
Postie defaults: slice 10 MiB, 5% redundancy, 1 GiB random file, Apple M4 10 cores (NEON path only), medians of 5 interleaved runs.
Run from tests/bench/nyuu after `go test -c -o work/pipeline.test ./pkg/postie`: `python3 ../rust-par2/compare.py 5`.
| tool | wall ms | MiB/s | CPU s | peak RSS MiB |
| --- | ---: | ---: | ---: | ---: |
| par2go (Go, via BenchmarkPipeline/par2-only) | 1301 | 787 | 3.77 | 596 |
| par2-rs 0.13.0 (Auto kernel, default memory) | 2100 | 488 | 2.62 | 1335 |
Both: 103 source slices, 5 recovery slices. par2-rs uses ~1.25 cores (CPU 2.62 s over 2.1 s wall): mostly serial.
Not tested: kernel/memory tuning, multi-file sets, x86 (GFNI/AVX2) paths, parmesan-par2, output cross-verification with par2cmdline.

## Diagnostic (2026-11-19): where par2-rs spends its 2.1 s
Phase timings via the progress callback (1 GiB, Postie defaults): SourceScan (read + hash) ~2000 ms, RecoveryEncode ~60 ms. GF16 work at 5% redundancy is negligible, so GF kernel choice is not the bottleneck.
KERNEL=simd, MEM_MB=256 and WEAVER_PAR2_CREATE_THREADS=10 all gave the same 2.0-2.1 s. CPU 2.45 s user over 2.1 s wall: the scan is effectively serial.
Reading: ~500 MB/s scan = roughly two MD5 passes (whole-file MD5 + per-slice MD5) at ~1 GB/s each on one thread. par2go reaches ~790 MiB/s by running the full-file MD5 on its own goroutine, leaving the single-stream MD5 floor.
So a ParPar GF16 FFI would not help; the lever is the scan pipeline.
