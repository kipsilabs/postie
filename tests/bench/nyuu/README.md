# Upload benchmark: Postie vs Nyuu

Uploads one file with Postie's CLI, with [Nyuu](https://github.com/animetosho/nyuu) and optionally with
another `postie` binary (`POSTIE_GO_BIN`), against the same in-memory NNTP sink (`sink/`, the
`javi11/nntp-server-mock` protocol handler with a discarding backend). PAR2 and post-check are off
(`postie.yaml`), so every client does the same work: read, yEnc, POST over 20 connections in 750 000-byte
articles. Each run reports wall time, MiB/s, client CPU and the sink's article/byte counts; the script ends
with the medians.

```bash
go build -o work/nntp-sink ./sink
(cd ../../../rust && cargo build --release -p postie-cli)
bun add nyuu@0.4.2 && bun pm trust --all
./bench.sh 1024 5 0                                   # 1 GiB, 5 reps, plain TCP
POSTIE_GO_BIN=/path/to/postie-v0.0.35 ./bench.sh 1024 5 1   # TLS, with the Go-era CLI too
```

Results and how to read them: [docs/docs/benchmarks.md](../../../docs/docs/benchmarks.md).
