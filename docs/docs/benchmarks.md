---
sidebar_position: 10
---

# Benchmarks

Two reproducible benchmarks ship with Postie, so performance can be measured instead of guessed:

- **Upload** (`tests/bench/nyuu/`): uploads the same file with Postie's CLI, with
  [Nyuu](https://github.com/animetosho/nyuu) and, optionally, with another Postie binary, against one
  in-memory NNTP server. It reports wall time, throughput and client CPU.
- **PAR2** (`rust/core/examples/par2_bench.rs`): times the built-in PAR2 creator on one file.

All numbers below were measured on 2026-11-24 on an Apple M4 (10 cores) over loopback, with the input in the
page cache. They compare tools and code changes; they do not predict your provider's speed. A real upload
is bound by your uplink, and then the useful question is how much CPU each tool spends to fill it.

## Upload

PAR2 and post-check are off for every client, so all of them do the same work: read, yEnc-encode and POST
one 1 GiB file of random data over 20 connections in 750 000-byte articles. The server is the
`javi11/nntp-server-mock` protocol handler with a discarding backend and a raw body reader, so it is not the
bottleneck. Every run is checked against the server's counters: 1432 articles each.

Clients: Postie 0.1.0 (this release), Postie v0.0.35 (the last Go release, built from its tag), and Nyuu
0.4.2 with its native yEnc module. Runs are interleaved; medians of 5.

| Mode | Client | Wall time | Throughput | Client CPU |
|------|--------|----------:|-----------:|-----------:|
| plain TCP | Postie 0.1.0 | 217 ms | 4.6 GiB/s | 1.14 ms/MiB |
| plain TCP | Postie v0.0.35 (Go) | 270 ms | 3.7 GiB/s | 1.28 ms/MiB |
| plain TCP | Nyuu 0.4.2 | 449 ms | 2.2 GiB/s | 0.50 ms/MiB |
| TLS | Postie 0.1.0 | 248 ms | 4.0 GiB/s | 1.22 ms/MiB |
| TLS | Postie v0.0.35 (Go) | 316 ms | 3.2 GiB/s | 1.46 ms/MiB |
| TLS | Nyuu 0.4.2 | 623 ms | 1.6 GiB/s | 0.67 ms/MiB |

How to read it:

- **0.1.0 is faster than the Go version and uses less CPU:** 25% (plain) to 28% (TLS) more throughput and
  11% to 16% less CPU per byte.
- **Postie finishes 2 to 2.5 times faster than Nyuu here,** and TLS costs it 13% of its throughput against
  Nyuu's 28%.
- **Nyuu still uses the least CPU per byte,** about half of Postie's. Nyuu runs as one Node process on
  roughly one core; Postie spreads the work over several cores and finishes sooner. On a one- or two-core
  machine, CPU becomes the limit and the gap narrows or flips. On a typical home connection, all three run
  at line rate and the difference is CPU load, not speed.

### Running it

```bash
cd tests/bench/nyuu
go build -o work/nntp-sink ./sink
(cd ../../../rust && cargo build --release -p postie-cli)
bun add nyuu@0.4.2 && bun pm trust --all
./bench.sh 1024 5 0   # 1 GiB, 5 repetitions, plain TCP
./bench.sh 1024 5 1   # the same over TLS
```

Set `POSTIE_GO_BIN` to another `postie` binary to add it to the comparison. `FAST=0` makes the server parse
each article line by line; every client then hits the server's own limit of about 1 GiB/s, which shows how
a slow server hides client differences.

## PAR2 creation

One 1 GiB file, 10 MiB slices, 5% redundancy (103 source slices, 6 recovery slices). Postie's built-in
creator against par2cmdline 1.3.0 from Homebrew, interleaved; medians of 3. The set Postie writes verifies
with par2cmdline.

| Tool | Wall time | CPU time |
|------|----------:|---------:|
| Postie 0.1.0 (built-in) | 1.39 s | 3.35 s |
| par2cmdline 1.3.0 | 5.33 s | 5.85 s |

The Homebrew par2cmdline build runs mostly on one core here, so treat it as a baseline rather than the
fastest PAR2 tool. Postie's limit is the whole-file MD5 that the PAR2 format requires: it is one sequential
stream (about 840 MB/s on this CPU, measured with `rust/core/examples/md5_speed.rs`), so at 1 GiB it
takes about 1.3 s of the 1.4 s whatever the core count. The recovery maths runs on
the other cores beside it.

Because Postie creates PAR2 while the file uploads (`posting.wait_for_par2: false`, the default), most of
this time overlaps the upload instead of adding to it.

```bash
cd rust
cargo build --release -p postie-core --example par2_bench
./target/release/examples/par2_bench /path/to/file /tmp/out.par2 10485760 5   # slice bytes, percent
```

## Making it more realistic

These runs use loopback, a cached input file and every core. To get closer to a real deployment:

- **Add latency and a bandwidth cap:** `tc qdisc add dev lo root netem delay 20ms rate 1gbit` on Linux,
  ideally inside Docker.
- **Limit the CPU:** run the clients under `docker run --cpus=2` or `taskset` to model a NAS.
- **Use a cold input:** a file larger than RAM, or drop the page cache between runs.
- **Run the whole job:** folder uploads with PAR2 and post-check on, at 5 to 20 GB.
- **Calibrate once against a real provider** in a test group. If the emulated setup reproduces the real
  ratio, trust it for iteration.
