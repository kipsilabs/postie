# Port map: postie (Go + Wails + SvelteKit) -> Rust

Goal: speed + full rewrite in Rust, crate-first (see `DECISIONS.md` ADR-001..008). Plan source: `rust-anything analyze --whole-app --lang go`.
Resume: read this file, then `HANDOFF.md` if present.

## Research (rust/research/)
written 2026-11-18, unverified by me beyond agent reports: `research/nntp.md`, `yenc-nzb.md`, `par2.md`, `app-stack.md`. Pending user decisions on their recommendations (not yet ADRs).

## Units (state: open | claimed by <session> <date> | done)
wave 0 (all done except frontend client swap): frontend/ (keep: Tauri client swap), pkg/fileinfo [trivial struct, ported with poster], config [done 2026-11-19: 59 Go load cases + duration/address oracles in rust/core], internal/apikey [done 2026-11-19, rust/core], article [done 2026-11-19: gate pass, 2269/2269 recorded cases; crates rust/article-rs, rust/yenc, rust/rapidyenc-sys], config, duplicatelookup, pausable, progress [done 2026-11-19, rust/core], transferstore [done 2026-11-19, rust/core], webauth [done 2026-11-19, rust/core; Go<->Rust token+hash interop verified], wshub [done 2026-11-19, rust/core; axum socket driver lives in cmd/web unit] — all open
wave 1: internal/arr [done 2026-11-19, rust/core: REST client replaces starr; mock-server tests], database [done], manifest [done], nzb [done], par2 [done], pool [done 2026-11-19, rust/core::pool + dynamic nntp Client]
wave 2: par2sweep [done 2026-11-19], poster [done 2026-11-19, rust/core::poster + engine/throttle/uploadmeter/nxg; 25 poster tests, 133 core tests], queue [done 2026-11-19, rust/core: goqite + queue, 18 listing cases diffed against Go over the same DB], transfercleaner [done 2026-11-19], transferwriter [done 2026-11-19], verification [done 2026-11-19, rust/core: 16 tests with real store+manifests] — open (mocks -> Rust test mocks)
wave 3: watcher [done 2026-11-19, rust/core::watcher + Go filepath.Match port], pkg/postie [done]
wave 4: processor [done 2026-11-19, rust/core::processor]
wave 5: cmd/postie, internal/backend — open
wave 6: cmd/web, main.go — open
dropped: tests/bench/nyuu/sink

## Steps
- [x] research subagents (see Research); requirement: pipelined POST and pipelined STAT (own window impl.)
- [x] `app record --http` on cmd/web: 232 cases (22 hand-picked API requests + 210 traps), Go replay 232/232 (2026-11-18). Build needs a stub `frontend/build/index.html` (gitignored); run with hermetic HOME/XDG dirs. Noise: `05-get-api-logs` body (lines) — ask user if log content is contract.
- [x] front door (axum) scaffolded in rust/app (binary currently named `home`: rename to postie); replay 232/232, fallback 100%; original launched via rust/original/run.sh (build: go build -o rust/original/postie-web ./cmd/web)
- [ ] units by wave, checkpoint after each (PAR2: see ADR-016; upload path first)
- [ ] lift

## Open questions
- Web API wire-compat beyond frontend use (assumed full).
- ADR-011, 013, 014 accepted by default via "do option 1": confirm or veto.

## NNTP
- `rust/nntp` (postie-nntp): done 2026-11-19, 18 tests, bench on par with prototype. Remaining: pool Manager glue (UpdateConfig with incremental provider add/remove, metrics shape), wired in by the poster/verification ports.

- par2: native creator, existing-set detection, native executor and scheduler [done 2026-11-19, rust/core/src/par2; 1 GiB at par2go parity]; BinaryExecutor [done]
