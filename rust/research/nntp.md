# NNTP client/pool: replacement for nntppool/v4 (research, 2026-11-18)

Sources are primary (crates.io API, GitHub repos/raw source, docs.rs). Details not visible in the fetched excerpt are marked "unverified".

## What Postie uses from nntppool (local code)
- `internal/config/config.go:867-935` (`ServerConfigToProvider`): per-provider Host:Port, Auth, `Connections`, `Inflight` (default 10), `IdleTimeout` (300s), `KeepAlive` (3600s TTL), TLS (`InsecureSkipVerify`, `ServerName`), custom `Factory` for SOCKS5 then TLS.
- `config.go:958,975`: `NewClient(providers, WithDispatchStrategy(DispatchRoundRobin))`, two clients (verify pool, upload pool).
- `internal/mocks/nntpclient.go`: the used surface is `PostYenc(headers, body, rapidyenc.Meta)`, `Stat`, `StatMany(ids, opts)` (streaming channel), `Stats()` (`ClientStats`/`ProviderStats`), `AddProvider`, `ErrArticleNotFound`, `TestProvider`.
- Not used: download/BODY, IHAVE, priorities, backup tiers.
- nntppool semantics to keep: pipelined `Inflight` POSTs per connection, STAT pipelining, weighted round-robin by free capacity, per-provider stats, 430 as a normal outcome (`ErrArticleNotFound`), 502 removal with re-add. Source: https://github.com/javi11/nntppool

## Candidates

### 1. nzb-nntp 0.2.26
- Maintenance: crates.io updated 2026-10-11, 14 versions, 2,493 downloads, created 2026-04-08 (https://crates.io/api/v1/crates/nzb-nntp). Latest GitHub commit 2026-10-11 (https://api.github.com/repos/TheDancingDeveloper-org/nzb-nntp/commits?per_page=1). 1 star. A mirror ("sync from rustnzb monorepo"), so the real development is elsewhere.
- License: MIT. Deps: tokio, tokio-rustls/rustls (ring), tokio-socks, governor, socket2, arc-swap (https://raw.githubusercontent.com/TheDancingDeveloper-org/nzb-nntp/main/Cargo.toml).
- Features (README, https://github.com/TheDancingDeveloper-org/nzb-nntp): per-server pools with health checks, idle timeout and reconnect; priority-based server order (0 first) with failover; a penalty/backoff on auth, timeout and connection errors; SOCKS5 with auth; global bandwidth limit; AUTHINFO; pipelined ARTICLE/STAT (batches of 100).
- Source (`src/connection.rs`, https://raw.githubusercontent.com/TheDancingDeveloper-org/nzb-nntp/main/src/connection.rs): has `post_article_bytes` (binary-safe) and `stat_article(message_id)`. HEAD is by article number only. No IHAVE. README lists no POST at all, so POST is a side feature.
- Gaps vs Postie: POST is not pipelined (the pipeline module covers ARTICLE/STAT). It is download-oriented. No per-provider `Inflight` POST pipelining. API is shaped for downloading.
- Perf traps: `post_article_bytes` does one `write_all` per line, and `Transport::write_all` always flushes. This gives one flush per line, which for a ~750 KB article with ~100k lines is a syscall/TLS-record storm. The reader has a 256 KiB `BufReader`, but there is no `BufWriter` (same source). It sets TCP_NODELAY. This is the same class of problem as the nntppool 4 KiB writer note in our memory.
- Semantic traps: `stat_article` returns `ArticleNotFound` without touching the socket if the server lacks STAT in CAPABILITIES. That would be a false "missing" and trigger reposts. Verify against our "transient error must not burn repost budget" fix.

### 2. pesto-poster 0.11.0 (library name `pesto`, module `crates/pesto/src/nntp/`)
- A Usenet *poster* (like Postie). It is published as a lib, "also provides the core library used by upapasta" (https://raw.githubusercontent.com/franzopl/pesto/main/crates/pesto/Cargo.toml).
- Maintenance: crates.io updated 2026-11-03, 59 versions, created 2026-05-19, 1,150 downloads (https://crates.io/api/v1/crates/pesto-poster). Latest commit 2026-11-04 (https://api.github.com/repos/franzopl/pesto/commits?per_page=1). 19 stars (https://github.com/franzopl/pesto). License MIT. Very young, a single author, and API churn is likely (59 releases in 6 months).
- Features (https://raw.githubusercontent.com/franzopl/pesto/main/crates/pesto/src/nntp/mod.rs): a `Connection` with `connect_with_proxy` (tokio-socks SOCKS5 *before* TLS, then tokio-rustls); `post_parts(headers, body)` that avoids copying the body and treats 441 as success; `repost_parts_confirmed` that only accepts 240; pipelined POST (`enqueue_post`/`flush_pipeline`/`read_post_response`); `stat` plus pipelined STAT; `head`; `body`; `mode_reader` keepalive; byte counters; every read/write/flush/connect/handshake bounded by a timeout. Stream is `BufReader<BufWriter<Box<dyn Stream>>>`, so writes are buffered.
- Pool (`pool.rs`, https://raw.githubusercontent.com/franzopl/pesto/main/crates/pesto/src/nntp/pool.rs): slot-per-worker with a fixed worker count. Servers are filled in list order up to their quota. On error the slot rotates to the next server. Reconnect is lazy with a fixed per-server delay (no exponential backoff). A `ConnectionBroker` semaphore caps total sockets. No weighted round-robin, no per-provider stats seen. It is a poster pool, not a general client.
- Gaps: no IHAVE, no throttling in `nntp/` (a rate limit may live elsewhere; unverified), no AUTH detail (file `auth.rs` exists, unread), and the pool is coupled to pesto's worker model. The caller must invalidate a connection after an error. The pipeline is per-connection, not an `Inflight`-style shared window.
- Semantic traps it documents: 441 on a repost can be a "ghost" article, so only an explicit 240 counts (mod.rs); STAT can say 223 while HEAD/BODY says 430 (mod.rs). Both matter for our verification path.
- Perf: the author's benchmark claims 2226 MiB/s post-only against a local mock server, versus 1339 for Nyuu. At 30 ms latency they converge at ~92 MiB/s (https://github.com/franzopl/pesto). Mock-server numbers only, not real TLS providers. Re-measure.

### 3. rek2_nntp 2.0.0
- Updated 2026-11-15 (https://crates.io/api/v1/crates). Async/tokio, RFC 3977/4643, has `post_article`, `ihave`, `takethis`, `check`, `stat`, `head`, TLS/STARTTLS, SASL, COMPRESS (https://docs.rs/rek2_nntp/latest/rek2_nntp/).
- License GPL-3.0-or-later, which is a blocker for a permissively licensed app unless Postie is GPL-compatible (check). No pool, no proxy mentioned, ~30% documented, native-tls and rustls both pulled in. Single-connection client only.

### 4. Others (rejected)
- nntp-rs 0.3.0 (MIT, last updated 2026-02-10): bb8 pool and rustls, but POST/IHAVE are "out of scope (by design)". https://github.com/jvz-devx/nntp-rs, https://crates.io/api/v1/crates/nntp-rs
- nntp-proxy 0.6.0 (MIT, 2026-11-05): a proxy server, not a client library. https://crates.io/api/v1/crates/nntp-proxy
- nntp 0.0.5 (2017), brokaw 0.2.0 (2020), rust-nntp (2015): abandoned (https://crates.io/api/v1/crates). melib: email client with an NNTP backend, not suitable.
- nzb-dispatch / nzb-news (same author as nzb-nntp): download-oriented.

## Coverage matrix (verified from sources above)
| Need | nzb-nntp | pesto | rek2_nntp | nntp-rs |
|---|---|---|---|---|
| async pool + rustls | yes | yes (custom) | no pool | yes (bb8) |
| multi-provider + priority | priority + penalty | order + rotate | no | no |
| AUTH | yes | yes | yes (SASL) | n/r |
| POST | yes, unbuffered per line | yes, pipelined, buffered | yes | no |
| IHAVE | no | no | yes | no |
| STAT/HEAD by id | STAT yes, HEAD by number | both | both | HEAD |
| pipelining | ARTICLE/STAT | POST + STAT | n/r | n/r |
| SOCKS5 | yes | yes | n/r | n/r |
| throttle | global bandwidth | not in nntp/ | n/r | no |
| backoff | penalty | fixed delay | no | no |

## Recommendation: write our own thin pool on tokio + tokio-rustls, borrowing from pesto
No crate covers the Postie contract: pipelined POST with a per-connection `Inflight` window, streaming StatMany, weighted RR, per-provider stats, idle/TTL, shared throttle, 502 removal. ADR-003 permits writing our own when no crate fits, and the two closest crates fail on specifics:
- nzb-nntp is download-first, has an unbuffered per-line POST, and has a STAT-capability trap.
- pesto's NNTP layer is the closest in behavior but is an internal module of a 0.x poster with its own worker coupling, so depending on `pesto-poster` pins us to its CLI-sized dependency tree (reqwest, rayon, sysinfo, PAR2).

Plan:
1. Build crate `postie-nntp`: `Connection` (tokio TcpStream, optional `tokio-socks` Socks5Stream, then `tokio-rustls`, `BufWriter` of 64 KiB+, TCP_NODELAY, timeouts on every IO), `Provider`, `Client`.
2. Adapt pesto's connection semantics (MIT; reuse with attribution): the 441 vs 240 distinction, the 223/430 STAT vs HEAD mismatch, and pipelined enqueue/flush/read.
3. Reimplement nntppool behaviors we rely on (weighted RR by free inflight, StatMany stream, `ClientStats`/`ProviderStats`, `ErrArticleNotFound` as a normal result, idle/TTL, 502 removal and re-add). Use `governor` (as nzb-nntp does) for the SharedThrottle.
4. Per ADR-003, benchmark against `pkg/postie/bench_test.go` and the pesto mock-server numbers. Add a fault-injection mock (like nzb-nntp's `tests/fault_injection.rs`) for the Go regression tests (timeout, STAT transient errors).
5. Keep SOCKS5-before-TLS ordering identical to the Go factory.

Open items: confirm Postie's own license versus GPL (rek2_nntp); read pesto `auth.rs`/`tls.rs` and check whether it has a throttle elsewhere; confirm nzb-nntp has no POST pipelining by reading `src/pipeline.rs`.

## Addendum (2026-11-18): mjc/nntp-proxy (suggested by the user)
Sources: https://github.com/mjc/nntp-proxy (MIT, v0.6.0, pushed 2026-11-15, 23 stars), src/lib.rs, src/client/mod.rs, src/pool/{mod,provider,deadpool_connection}.rs.
- Has a published lib (`nntp_proxy`) with a deadpool-based `DeadpoolConnectionProvider` (`with_tls_auth`), buffer pool, health checks, prewarming, graceful shutdown, multi-backend routing, socket2 keepalive/nodelay, and a standalone `client::NntpClient` (`fetch_body`, `stat`).
- Reader/download oriented: in the files read, no POST/IHAVE send path, no pipelined posting, no SOCKS5 (grep of those files only; not a full-repo audit; code search API returned nothing).
- Requires Rust 1.95, edition 2024; lib docs call the API "evolving".
- Conclusion: strong source/dependency candidate for the pool, TLS, auth, health-check and prewarm layers and for STAT; posting (pipelined POST window, 240/441 handling, throttle, SOCKS5-before-TLS) must still be added. Not a drop-in for nntppool.

## Requirement (user, 2026-11-18): pipeline STAT as well as POST
- `nntp_proxy::client::NntpClient::stat` (src/client/mod.rs:150) does one checkout + one request/response exchange per call: not pipelined. Same for fetch.
- So pipelined STAT (a window of N outstanding STATs per connection, responses matched in order, streaming results like nntppool's `StatMany`) must be implemented by us, alongside the pipelined POST window. Both share one per-connection in-flight window design.
- Traps: 223 from STAT can still be 430 on HEAD/BODY; a server without STAT in CAPABILITIES must not be reported as "not found" (nzb-nntp trap); on connection drop, requeue all unanswered in-flight STATs, never treat them as missing.

## Addendum 2 (2026-11-19): infinidysk/infinidysk (suggested by the user)
Sources: https://github.com/infinidysk/infinidysk (MIT, created 2026-02-25, pushed 2026-11-19, 307 stars); workspace Cargo.toml; crates/nntp/src/{client,connection,pool}.rs; crates/yenc/src/lib.rs + Cargo.toml; crates/par2/src/lib.rs.
- Rust Usenet *streaming* app (NzbDAV successor). Workspace crates: nntp, yenc, par2, queue, health, etc. All crates `publish = false`, version 0.0.0, rust-version 1.99, edition 2024: usable only as a git dependency or by copying (MIT).
- `infinidysk-nntp` (tokio + tokio-rustls + webpki-roots + socket2): connection has pipelined `send_stat`/`read_stat` and `send_body`/`read_body` with explicit `flush`; pool has `pipeline_depth`, prewarm, circuit breaker (`breaker.rs`), bandwidth limit, per-provider stats, multi-provider client. This is the pipelined STAT the user asked for, already implemented (download side).
- No POST/IHAVE and no SOCKS5 seen in client/connection/pool (grep of those files; not a full audit).
- `infinidysk-yenc`: rapidyenc FFI via git submodule + cmake build. Exposes `encode(src, line_size)`, `decode`, `decode_incremental`, `crc32`. So rapidyenc *encode* is available from Rust (contradicts yenc-nzb.md "no binding"; that note only covered crates.io/permissive crates). Uses `unsafe` FFI; needs cmake and C++ toolchain.
- `infinidysk-par2`: packet parsing and slice verification only (md-5). No recovery-set creation: not a replacement for par2go/par2-rs.
- Conclusion: best reference/source for pipelined STAT window, pool, breaker, prewarm and the rapidyenc binding; posting path, SOCKS5, throttle still ours. Must copy or vendor (not on crates.io).

## Addendum 3 (2026-11-19): nzbfast/nzbfast (suggested by the user) — LICENSE BLOCKER
Sources: https://github.com/nzbfast/nzbfast (README, COPYRIGHT.md, LICENSE; releases parfast-v1.10.0, v1.10.0), crates/{postfast,parfast}/Cargo.toml.
- Rust Usenet downloader (SABnzbd/NZBGet API), created 2026-07-21, pushed 2026-11-19, 161 stars. **License: GPL-3.0-or-later.** Postie is MIT (LICENSE: "MIT License, Copyright (c) 2024 Javi").
- Claims pipelined NNTP (many requests in flight per connection), native PAR2 verify/repair, native RAR, bundled rapidyenc.
- `parfast` crate/CLI: "PAR2 create, verify and repair with par2cmdline's command shape and exit codes"; separate releases (v1.10.0, 2026-11-10) with attestations/SBOM. `publish = false`. Native Rust create exists here (unlike infinidysk-par2).
- `postfast` crate: a posting-layout generator for its own test oracle, not a poster engine.
- Consequence: linking or vendoring any nzbfast/parfast code into Postie would require Postie to become GPL-3+. Using the `parfast` binary as a separate process (like par2cmdline) or only reading its design for ideas does not. Not verified legally; user to decide.
- Useful as a PAR2 cross-verification oracle (par2cmdline-compatible exit codes) next to par2cmdline-turbo.
