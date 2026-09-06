---
sidebar_position: 8
---

# Benchmarks

Postie ships two reproducible benchmarks so upload performance can be measured instead of guessed:

- **Pipeline benchmark** (`pkg/postie/bench_test.go`): runs the real Postie pipeline (PAR2 → yEnc → NNTP POST) against an in-process sink server. It isolates Postie's own CPU and I/O work from the network.
- **Nyuu comparison** (`tests/bench/nyuu/`): uploads the same file with Postie's CLI and with [Nyuu](https://github.com/animetosho/nyuu) against the same in-memory NNTP server, and records wall time, throughput, and client CPU.

All numbers below were taken on 2026-09-06 on an Apple M-series laptop (10 cores) over loopback. They are relative measurements for comparing code changes and tools, not a prediction of what your provider will give you: a real upload is bounded by your uplink, and the interesting question then becomes how much CPU each tool burns to fill it.

## Pipeline benchmark

```bash
POSTIE_BENCH_MB=1024 POSTIE_BENCH_TLS=1 \
  go test ./pkg/postie -run '^$' -bench Pipeline -benchtime 1x -count 5
```

Knobs: `POSTIE_BENCH_MB` (input size, default 256), `POSTIE_BENCH_CONNS` (connections, default 20), `POSTIE_BENCH_TLS=1` (serve the sink over TLS, as real providers do), `POSTIE_BENCH_PAR2_THREADS` (PAR2 GF16 threads, default auto).

Sub-benchmarks: `par2-only`, `upload-only`, `full/wait_for_par2`, `full/parallel_par2`, `folder/wait_for_par2`, `folder/parallel_par2`.

### Where the time goes

| Stage, 256 MiB, 20 connections | Time | Throughput |
|---|---|---|
| PAR2 creation only | 0.42 s | ~640 MB/s |
| Upload only, plain TCP | 0.06 s | ~4.2 GiB/s |
| Upload only, TLS | 0.13 s | ~2.0 GiB/s |
| Full pipeline, `wait_for_par2: true` | 0.51 s | ~500 MiB/s |
| Full pipeline, `wait_for_par2: false` | 0.49 s | ~525 MiB/s |

Two conclusions from the CPU profiles:

1. **PAR2 creation is bound by the whole-file MD5.** At 256 MiB, 1 GiB, and 2 GiB the PAR2 stage stays at 600 to 650 MB/s and 60 to 70% of CPU samples are in `crypto/md5`. The PAR2 format requires an MD5 of the entire file, which is a single sequential stream; the per-slice hashes and the GF16 recovery math run in parallel and are not the limit for files under several GB. The GF16 thread count has no measurable effect.
2. **TLS uploads were dominated by write syscalls.** With a 4 KiB connection write buffer, each ~780 KiB encoded article became ~190 TLS records; 88% of TLS upload CPU was in write syscalls.

### Fixes and their effect

Both fixes live in Postie's own libraries and are measured here with the pipeline benchmark, 1 GiB over TLS, medians of 3:

| Benchmark | Before | After |
|---|---|---|
| PAR2 only | 1740 ms | 1431 ms |
| Upload only | 464 ms | 354 ms |
| Full, `wait_for_par2: true` | 2243 ms | 1799 ms |
| Full, `wait_for_par2: false` | 2220 ms | 1745 ms |
| Folder, `wait_for_par2: true` | 2385 ms | 1793 ms |

- **nntppool: 64 KiB connection write buffer** ([javi11/nntppool#102](https://github.com/javi11/nntppool/pull/102)). Upload-only at 1 GiB, median of 5: TLS 471 → 291 ms (1.6x), plain TCP 324 → 240 ms.
- **par2go: whole-file MD5 pipelined off the reader goroutine.** Reading, hashing, and feeding the GF16 encoder used to run serially on one goroutine per file; the hash now runs on its own goroutine. PAR2 at 1 GiB: 1646 → 1466 ms. The remaining time is the single-stream MD5 floor (~880 MB/s on this CPU).

## Postie vs Nyuu

```bash
cd tests/bench/nyuu
go build -o work/nntp-sink ./sink
(cd ../../.. && go build -o tests/bench/nyuu/work/postie-cli ./cmd/postie)
bun add nyuu && bun pm trust --all
./bench.sh 1024 5 0   # 1 GiB, 5 reps, plain TCP
./bench.sh 1024 5 1   # same over TLS
```

Both tools upload one 1 GiB file of random data over 20 connections with 750 000-byte articles. PAR2 and post-check are off on both sides so the work is identical: read, yEnc, POST. The server is the `javi11/nntp-server-mock` protocol handler with a discarding in-memory backend and a raw body drain, so it is not the bottleneck. Every run is validated by the server's article and byte counters (1432 articles each).

Versions: Postie at nntppool `v4.22.3` (PR #102), Nyuu 0.4.2 with its native yencode module.

### Results, medians of 5

| Mode | Client | Wall time | Throughput | Client CPU |
|---|---|---|---|---|
| plain TCP | Postie | 333 ms | 3.0 GiB/s | 1.45 ms/MiB |
| plain TCP | Nyuu | 471 ms | 2.1 GiB/s | 0.55 ms/MiB |
| TLS | Postie | 337 ms | 3.0 GiB/s | 1.45 ms/MiB |
| TLS | Nyuu | 643 ms | 1.6 GiB/s | 0.70 ms/MiB |

### How to read it

- **Postie is faster on this machine**: 1.4x over plain TCP and 1.9x over TLS, and TLS barely slows it while it costs Nyuu a third of its throughput.
- **Nyuu is more CPU-efficient.** Nyuu is a single Node process running at about 1.2 cores, so it is CPU-bound on one core. Postie spreads the work across ~4.5 cores and finishes sooner, but spends about 2.6x more CPU per byte.
- **What that means for you.** On a desktop, server, or multi-core NAS Postie will be faster. On a one- or two-core box the ranking may flip, because there Postie's CPU per byte becomes the limit. On a typical home uplink both tools sit at line rate and the difference is CPU load, not speed.

Postie's extra CPU is per-article allocation and hand-off overhead in the POST path (encoder buffer growth per article, the pipe between encoder and connection writer, one read-ahead goroutine per article), not the yEnc kernel or the socket writes. Both tools use SIMD yEnc encoders.

### With the mock's textproto parser

`FAST=0 ./bench.sh 1024 3 0` runs the same comparison through the mock server's line-by-line article parser instead of the raw drain. Both clients then sit at the server's ~1 GiB/s ceiling and wall times are equal within noise; only the CPU column carries information. It is kept as a reminder that a slow server hides client differences.

## Making the test more realistic

The benchmarks above run over loopback with the input file in the page cache and all cores available. To approximate a real deployment:

- **Add latency and a bandwidth cap**: `tc qdisc add dev lo root netem delay 20ms rate 1gbit` on Linux, or `dnctl` plus a pf rule on macOS, ideally inside Docker.
- **Limit CPU**: run the clients under `docker run --cpus=2` or `taskset` to model a NAS.
- **Use a cold input**: a file larger than RAM, or drop the page cache between runs, on the kind of disk your users have.
- **Run the whole job**: folder mode with PAR2 enabled at 5 to 20 GB, with post-check on in both tools.
- **Calibrate once against a real provider** in a test group; if the emulated setup reproduces the real ratio, trust it for iteration.
