# postie vs Nyuu upload comparison

Uploads one file with postie's CLI and with [Nyuu](https://github.com/animetosho/nyuu)
against the same in-memory NNTP sink (the `javi11/nntp-server-mock` protocol
handler with a discarding backend, see `sink/`). PAR2 and post-check are off, so
both tools do identical work: read, yEnc, POST over 20 connections, 750 000-byte
articles. Reported per run: wall time, MiB/s, client CPU (user+sys), and the
sink's article/byte counts as proof of a complete upload.

```
go build -o work/nntp-sink ./sink
(cd ../../.. && go build -o tests/bench/nyuu/work/postie-cli ./cmd/postie)
bun add nyuu && bun pm trust --all
./bench.sh 1024 3 0   # 1 GiB, 3 reps, plain TCP
./bench.sh 1024 3 1   # same over TLS
```

## Results, 2026-09-06

Apple M-series, 10 cores, loopback. postie at nntppool
`v4.22.3` (PR javi11/nntppool#102, 64 KiB
connection writer), Nyuu 0.4.2 with native yencode.

### Fast sink (default, `FAST=1`)

```
# 1024 MiB, 20 connections, tls=0, fast-sink=1, arm64 10 cores
postie      464 ms   2207.7 MiB/s  cpu  1.53 s ( 1.49 ms/MiB)  sink:articles=1432 bytes=1108287800
nyuu        497 ms   2062.4 MiB/s  cpu  0.57 s ( 0.56 ms/MiB)  sink:articles=1432 bytes=1108312159
postie      329 ms   3112.7 MiB/s  cpu  1.41 s ( 1.38 ms/MiB)  sink:articles=1432 bytes=1108287800
nyuu        482 ms   2125.0 MiB/s  cpu  0.56 s ( 0.55 ms/MiB)  sink:articles=1432 bytes=1108312159
postie      333 ms   3075.6 MiB/s  cpu  1.52 s ( 1.48 ms/MiB)  sink:articles=1432 bytes=1108287800
nyuu        469 ms   2182.1 MiB/s  cpu  0.54 s ( 0.53 ms/MiB)  sink:articles=1432 bytes=1108312159
postie      360 ms   2843.2 MiB/s  cpu  1.53 s ( 1.49 ms/MiB)  sink:articles=1432 bytes=1108287800
nyuu        471 ms   2174.3 MiB/s  cpu  0.56 s ( 0.55 ms/MiB)  sink:articles=1432 bytes=1108312159
postie      316 ms   3243.2 MiB/s  cpu  1.48 s ( 1.45 ms/MiB)  sink:articles=1432 bytes=1108287800
nyuu        465 ms   2201.6 MiB/s  cpu  0.54 s ( 0.53 ms/MiB)  sink:articles=1432 bytes=1108312159
# 1024 MiB, 20 connections, tls=1, fast-sink=1, arm64 10 cores
postie      336 ms   3047.8 MiB/s  cpu  1.47 s ( 1.44 ms/MiB)  sink:articles=1432 bytes=1108287800
nyuu        643 ms   1593.7 MiB/s  cpu  0.72 s ( 0.70 ms/MiB)  sink:articles=1432 bytes=1108312159
postie      335 ms   3057.2 MiB/s  cpu  1.47 s ( 1.44 ms/MiB)  sink:articles=1432 bytes=1108287800
nyuu        633 ms   1617.4 MiB/s  cpu  0.72 s ( 0.70 ms/MiB)  sink:articles=1432 bytes=1108312159
postie      361 ms   2832.7 MiB/s  cpu  1.52 s ( 1.48 ms/MiB)  sink:articles=1432 bytes=1108287800
nyuu        622 ms   1646.7 MiB/s  cpu  0.71 s ( 0.69 ms/MiB)  sink:articles=1432 bytes=1108312159
postie      337 ms   3036.8 MiB/s  cpu  1.49 s ( 1.46 ms/MiB)  sink:articles=1432 bytes=1108287800
nyuu        649 ms   1577.9 MiB/s  cpu  0.73 s ( 0.71 ms/MiB)  sink:articles=1432 bytes=1108312159
postie      502 ms   2038.3 MiB/s  cpu  1.68 s ( 1.64 ms/MiB)  sink:articles=1432 bytes=1108287800
nyuu        654 ms   1566.9 MiB/s  cpu  0.74 s ( 0.72 ms/MiB)  sink:articles=1432 bytes=1108312159
```

Medians: plain TCP postie 333 ms (3.0 GiB/s) vs Nyuu 471 ms (2.1 GiB/s), 1.4x;
TLS postie 337 ms (3.0 GiB/s) vs Nyuu 643 ms (1.6 GiB/s), 1.9x. The two
tools trade off differently: Nyuu is a single Node process and runs at about
1.2 cores, so it is CPU-bound on one core at ~0.55 ms/MiB (0.70 under TLS);
postie spreads the work over ~4.5 cores and finishes sooner but spends
~1.45 ms of CPU per MiB, roughly 2.6x Nyuu's. On a multi-core machine postie
is faster; on a one- or two-core box Nyuu's lower per-byte cost would win.
postie's extra CPU is per-article allocation and hand-off in the POST path
(encoder buffer growth per article in rapidyenc, the `io.Pipe` between encoder
and connection writer, one read-ahead goroutine per article), not the yEnc
kernel or the socket writes.

### Mock textproto parser (`FAST=0`)

```
postie      958 ms   1068.4 MiB/s  cpu  1.24 s ( 1.21 ms/MiB)  sink:articles=1432 bytes=1099418454
nyuu       1087 ms    942.3 MiB/s  cpu  0.90 s ( 0.88 ms/MiB)  sink:articles=1432 bytes=1099418469
postie      967 ms   1058.6 MiB/s  cpu  1.39 s ( 1.36 ms/MiB)  sink:articles=1432 bytes=1099418454
nyuu       1017 ms   1006.6 MiB/s  cpu  0.98 s ( 0.96 ms/MiB)  sink:articles=1432 bytes=1099418469
postie      931 ms   1099.4 MiB/s  cpu  1.35 s ( 1.32 ms/MiB)  sink:articles=1432 bytes=1099418454
nyuu        981 ms   1043.7 MiB/s  cpu  0.92 s ( 0.90 ms/MiB)  sink:articles=1432 bytes=1099418469
postie      852 ms   1201.7 MiB/s  cpu  1.38 s ( 1.35 ms/MiB)  sink:articles=1432 bytes=1099418454
nyuu       1071 ms    956.1 MiB/s  cpu  1.10 s ( 1.07 ms/MiB)  sink:articles=1432 bytes=1099418469
postie      833 ms   1229.4 MiB/s  cpu  1.31 s ( 1.28 ms/MiB)  sink:articles=1432 bytes=1099418454
nyuu        978 ms   1046.7 MiB/s  cpu  1.03 s ( 1.01 ms/MiB)  sink:articles=1432 bytes=1099418469
postie     1067 ms    959.7 MiB/s  cpu  1.38 s ( 1.35 ms/MiB)  sink:articles=1432 bytes=1099418454
nyuu       1274 ms    804.0 MiB/s  cpu  1.17 s ( 1.14 ms/MiB)  sink:articles=1432 bytes=1099418469
```

Here both clients sit at the server's ~1 GiB/s ceiling (the mock's dot-reader
parses each article line by line), so wall time is parity within noise and
only the CPU column carries information.
