# Port map: postie (Go + Wails + SvelteKit) -> Rust

Goal: speed + full rewrite in Rust, crate-first (see `DECISIONS.md` ADR-001..008). Plan source: `rust-anything analyze --whole-app --lang go`.
Resume: read this file, then `HANDOFF.md` if present.

## Research (rust/research/)
written 2026-11-18, unverified by me beyond agent reports: `research/nntp.md`, `yenc-nzb.md`, `par2.md`, `app-stack.md`. Pending user decisions on their recommendations (not yet ADRs).

## Units (state: open | claimed by <session> <date> | done)
wave 0: frontend/ (keep: Tauri client swap), pkg/fileinfo, internal/apikey, article [done 2026-11-19: gate pass, 2269/2269 recorded cases; crates rust/article-rs, rust/yenc, rust/rapidyenc-sys], config, duplicatelookup, pausable, progress, transferstore, webauth, wshub — all open
wave 1: internal/arr, database, manifest, nzb, par2, pool — open
wave 2: par2sweep, poster, queue, transfercleaner, transferwriter, verification — open (mocks -> Rust test mocks)
wave 3: watcher, pkg/postie — open
wave 4: processor — open
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
