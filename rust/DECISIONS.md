# Decisions (ADRs)

## ADR-001: Desktop shell is Tauri v2
Status: accepted (2026-11-18)
Context: Wails is Go-only. The SvelteKit frontend must be kept.
Decision: Replace Wails with Tauri v2; swap the Wails client in `frontend/src/lib/api/client.ts` for Tauri `invoke`/events. The axum web server stays as a second binary.
Consequences: Wails bindings (`wailsjs/`) are retired at the lift. Frontend keeps UI code unchanged otherwise.

## ADR-002: Scope
Status: accepted (2026-11-18)
Decision: Port every entry point (`main.go`, `cmd/web`, `cmd/postie`). Drop `tests/bench/nyuu/sink` (bench tool). `internal/mocks` (gomock) becomes Rust test mocks, not a unit.

## ADR-003: Crate-first policy
Status: accepted (2026-11-18)
Context: Goal is speed plus using available Rust libraries for NNTP, yEnc, PAR2, NZB.
Decision: Use a maintained Rust crate wherever research finds one. Write our own only when none exists or it benchmarks slower than the current Go path. `nntppool`/`par2go` are ported only if no crate wins. Candidates are benchmarked against the Go numbers in `pkg/postie/bench_test.go`.
Consequences: Each dependency gets `rust/research/<topic>.md` (primary sources only) before dependent units are ported.

## ADR-004: Durable queue
Status: accepted (2026-11-18)
Decision: Replace `goqite` with a small SQLite-backed queue on `rusqlite`, same schema, so the Rust app opens existing DBs.

## ADR-005: On-disk compatibility
Status: accepted (2026-11-18)
Decision: `config.yaml`, the SQLite DB with goose migrations 001-009, and NZB output stay compatible. No user-visible migration step on upgrade.
Consequences: Migrations are reimplemented against the same `goose_db_version` table.

## ADR-006: Equivalence bar
Status: accepted (2026-11-18)
Decision: Byte-identical NZB output; identical PAR2 output where deterministic; identical HTTP status codes and JSON bodies for the web API. Normalize only timestamps, ids and log lines. Noise sorted as `order`/`lines` by `app record` is raised with the user, never accepted silently.
Open: whether the web API must stay wire-compatible beyond what the frontend uses (asked; assume full compatibility until told otherwise).

## ADR-007: Targets
Status: accepted (2026-11-18)
Decision: macOS, Windows, Linux; keep Docker image and Homebrew formula. PAR2 SIMD paths are tested on non-AVX512 CPUs (see Zen 4 GFNI+AVX512 segfault note) and need a runtime-selectable kernel.

## ADR-008: Checkpoints and cutover
Status: accepted (2026-11-18)
Decision: Stop for user approval after each wave. Run the Rust app beside the Go app first (axum front door proxies unported routes to Go); no big-bang switch. Kill switch `RUST_ANYTHING_UNIT_<NAME>=original` until the lift.

## ADR-009: Go-ahead after the hot-path prototype
Status: accepted (2026-11-19)
Context: `tests/bench/rust-post` (4 GiB, 20 conns, TLS, depth 10, medians of 5): Rust + C rapidyenc used 0.63 ms/MiB CPU vs Go's 1.57 (-60%), +10% throughput, ~40% less RSS; scalar yEnc alone was 36% worse on CPU. `tests/bench/rust-par2`: par2-rs 2100 ms vs par2go 1301 ms for 1 GiB.
Decision: Proceed with the full rewrite, upload path first. PAR2 speed is an open question (see ADR-013).
Consequences: Prototype caveats stay open: it omits SQLite state, NZB writing, retries and verification, and the measured gap is loopback only. Re-measure the real app against the same harness at each wave.

## ADR-010: yEnc via C rapidyenc over FFI
Status: accepted (2026-11-19) — supersedes the scalar-first leaning in ADR-003 for yEnc
Decision: Wrap C rapidyenc (animetosho, built with cmake, static link) behind one small `unsafe` module. Keep the scalar Rust encoder as the portable fallback and as a test oracle. CRC32 via `crc32fast`.
Consequences: Build needs cmake and a C++ toolchain (the Go app already needs C++ via cgo). Test non-AVX512 CPUs and a force-portable switch.

## ADR-011: NNTP pool
Status: accepted by default (2026-11-19, via "do option 1"; veto welcome)
Decision: Vendor (MIT notice kept) infinidysk's `nntp` crate as the base for pool, pipelined STAT/BODY, breaker, prewarm and TLS; add ourselves: pipelined two-phase POST (send POST, wait for 340, then body; depth per connection), 240/441 handling (only explicit 240 is success), shared throttle, SOCKS5 before TLS, weighted round-robin. `nntp-proxy` is a reference only. Pipelined STAT is required. Needs Rust 1.99 for vendored crates; prototype ran on 1.98.
Consequences: Own the posting path; track upstream by copying, not depending.

## ADR-012: Licensing guard
Status: accepted (2026-11-19)
Decision: nzbfast/parfast (GPL-3.0-or-later) are reference and test oracle only: no code vendored or linked, so Postie stays MIT. Running the parfast binary as a separate process in tests is allowed.

## ADR-013: PAR2
Status: accepted by default (2026-11-19); engine choice open
Decision: Gate is cross-verification: par2cmdline (and parfast as oracle) must verify and repair sets we create, and we must verify theirs; byte-identity is not required (supersedes the PAR2 part of ADR-006). Engine: first task is a ParPar C backend FFI benchmark (par2go wraps ParPar's C code) against par2go and par2-rs (par2-rs was slower: 2.1 s vs 1.3 s for 1 GiB). Fallbacks: par2-rs, parmesan-par2. Kernel must be runtime-selectable, with conservative default on AMD Zen 4 (earlier GFNI+AVX512 segfault).

## ADR-014: NZB and compatibility details
Status: accepted by default (2026-11-19); veto welcome
Decision: NZB writer is hand-written; segment ids and meta text are XML-escaped properly (deliberate difference from Go's raw output); `<meta>` order is fixed alphabetical; date and generated ids normalized in comparisons; segment `bytes` quirk (configured size for all but the last) kept. Fix inherited Go bugs: zstd levels 1-22 map to working encoder levels; `socks5://user:pass@host:port` is parsed. `GET /api/logs` is treated as free-form (recorded as a wildcard).
Consequences: ADR-006 byte-identical NZB now means "after normalizing date/meta order/escaping differences listed here".

## ADR-015: Migrations and queue facts (corrects ADR-004/005)
Status: accepted (2026-11-19)
Decision: There are ten goose migrations (001-010, `010_add_web_auth.sql`), not nine. Custom runner over the same embedded SQL keeps the `goose_db_version` table (rusqlite_migration and refinery are ruled out: they use their own version tracking). The queue is a hand-written rusqlite implementation of the same `goqite` table/SQL as migration 001: text timestamps `%Y-%m-%dT%H:%M:%S.fffZ` compared as strings, max 3 receives, atomic receive UPDATE ... RETURNING, JSON body fields unchanged.

## ADR-016: PAR2 engine plan after the diagnostic
Status: accepted (2026-11-19) — refines ADR-013
Context: par2-rs spends ~2.0 of 2.1 s in the source scan (hashing) and ~60 ms in GF16 encode at 5% redundancy; kernel, memory and thread knobs change nothing. par2go (ParPar GF16 + pipelined full-file MD5) takes 1.3 s. The floor for one large file is the single-stream whole-file MD5 (~0.9-1 GB/s on M4).
Decision: Drop the planned ParPar-GF16-FFI benchmark (GF is not the bottleneck). Target parity with par2go: hash the whole file and the slices on separate threads (or pair MD5 streams), then use a Rust PAR2 packet/encode path. Preferred route: patch or fork par2-rs (Apache-2.0) to pipeline its scan; fallback: own scan + par2-rs encode/packets; keep par2go behind the front door until parity is measured. Re-measure on x86 (GFNI/AVX2) and multi-file sets, and cross-verify with par2cmdline/parfast (ADR-013).
Consequences: PAR2 will not be faster than Go for one big file unless MD5 itself gets faster; the rewrite's speed case rests on the upload path (ADR-009).

## ADR-017: Stateful and IO units are ported as plain Rust libraries in `rust/core`
Status: accepted (2026-11-19)
Context: `rust-anything` records pure functions. Most remaining units are SQLite stores, goroutine/context code, HTTP handlers or filesystem code, where it finds few or no bridgeable exports (e.g. `apikey` exposes only a random `Generate`). The Go tests (79 files) are the executable spec for them.
Decision: Units without meaningful pure functions are ported into the `postie-core` crate (tokio, rusqlite) as ordinary libraries, each with Rust tests derived from the Go tests plus compatibility tests against databases and files created by the Go app. `rust-anything bridge/record/diff/gate` is still used for units with pure logic (article, nzb, config validation, etc.). HTTP behavior stays proven by the recorded app cases (`app replay`).
Consequences: Less automated differential proof for stateful units; compensate with Go-created fixture DBs, shadow runs in the front door, and replay at each wave.

## ADR-018: Config error texts and YAML dialect
Status: accepted (2026-11-19)
Context: `config.Load` was recorded against 59 YAML inputs (`rust/core/tests/fixtures/config-load.json`), plus Go oracles for durations and `net/mail` addresses. Rust matches the resulting config JSON and every validation/duration/address error text exactly.
Decision: YAML syntax and type errors (from `yaml.v3` vs `serde_yaml_ng`) differ in wording; they are compared only by the `error parsing config file: ` prefix. YAML 1.1 bool words (`yes/no/on/off/y/n`) are accepted for bool fields like yaml.v3, and integral floats (`563.0`) for integer fields. A config saved by Rust loads in Go. Likewise, the XML syntax-error texts of `ValidateNZB` were matched case by case and need no waiver.
Consequences: users editing config.yaml see the same validation messages; only malformed-YAML wording changes.
