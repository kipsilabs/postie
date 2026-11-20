# Postie Go -> Rust: application-stack research

Scope: goqite, goose, Wails -> Tauri v2, gorilla/mux + websocket -> axum, auth, *arr, log rotation, watcher, compression, SOCKS5, YAML.
Method: Go usage read from this repo and the Go module cache (primary source code); Rust claims fetched from docs.rs / crates.io / v2.tauri.app / GitHub on 2026-11-18.
Tag `[unverified]` = not confirmed on a primary page in this session (from prior knowledge); check before relying on it.

## Summary of recommendations

| # | Item | Choice |
|---|------|--------|
| 1 | goqite | Hand-written `queue` module on `rusqlite`, same `goqite` table + same SQL (no crate exists to adopt) |
| 2 | goose | Custom ~80-line runner that reads/writes `goose_db_version`; embed the same `.sql` files |
| 3 | Wails | Tauri v2 + same SvelteKit (adapter-static SPA); `axum` 0.8 for web mode; one shared core crate |
| 3 | mux / ws | `axum` Router + `axum::extract::ws` (feature `ws`) |
| 4 | Auth | `jsonwebtoken` (HS256) + `bcrypt` crate; both formats are wire-compatible |
| 5 | starr | No Rust crate; 3 hand-rolled `reqwest` calls |
| 5 | lumberjack | `file-rotate` (size + count + gzip); age cleanup done by hand |
| 5 | fsnotify | Not needed: Go watcher polls with `WalkDir`; use `tokio::time::interval` + `walkdir` |
| 5 | brotli/zstd | `brotli` + `zstd` crates; MUST map levels (see trap) |
| 5 | SOCKS5 | `tokio-socks` `Socks5Stream` + `tokio-rustls` |
| 5 | yaml | `serde_yaml_ng` |

## 1. goqite -> rusqlite

Go usage: `internal/queue/queue.go:155` `goqite.New(goqite.NewOpts{DB, Name: "file_jobs"})` (defaults: MaxReceive=3, Timeout=5s).
Operations used: `Send` (with `Priority`), `Receive`, `Delete`, `Extend` (`ExtendTimeout`). Postie also reads/writes the `goqite` table with raw SQL
(`queue_test.go:99` uses `json_extract(body,'$.path')`; `queue.go` ~1474 deletes by id), so table compatibility is mandatory, not optional.

Schema in the existing DB comes from migration 001 (NOT the upstream file): `internal/database/migrations/001_initial_schema.sql`
creates `goqite(id text pk default 'm_'||lower(hex(randomblob(16))), created, updated, queue, body blob, timeout, received integer, priority integer) strict`
plus indexes `goqite_queue_created_idx`, `goqite_queue_priority_idx`. Upstream `schema_sqlite.sql` additionally has an `updated` trigger and an index
`(queue, priority desc, created)`; Postie's migration deliberately omits the trigger ("goose parsing issues"), so `updated` is never maintained. Do not add the trigger in Rust.
Source: goqite module cache `maragu.dev/goqite@v0.3.2-0.20250625131501-cacb23e73698/{schema_sqlite.sql,goqite.go}`; repo https://github.com/maragudk/goqite

Exact semantics to replicate (from goqite.go):
- Timestamps are TEXT in Go layout `2006-01-02T15:04:05.000Z07:00` in UTC => Rust `%Y-%m-%dT%H:%M:%S%.3fZ`. Compared as strings, so format must match exactly (3 fractional digits, `Z`).
- Send: `insert into goqite (queue, body, timeout, priority) values (?,?,?,?) returning id`; `timeout = now + delay`.
- Receive (one atomic statement, run in a tx):
  `update goqite set timeout=?(now+queueTimeout), received=received+1 where id=(select id from goqite where queue=? and ?(now) >= timeout and received < ?(maxReceive) order by priority desc, created limit 1) returning id, body`.
  So "visibility timeout" = `timeout` column; "receive count" = `received`; a message received `maxReceive` (3) times without Delete is silently never delivered again (dead-letter by neglect).
- Extend: `update goqite set timeout=? where queue=? and id=?` (now + delay). Delete: `delete from goqite where queue=? and id=?`.
- Message IDs are `m_` + 32 hex chars; `body` is the JSON of `queue.FileJob` (Postie queries it with `json_extract`) so keep the serde field names identical.

Traps:
- `STRICT` tables and `RETURNING` need SQLite >= 3.37 / 3.35 `[unverified: sqlite.org/stricttables.html, lang_returning.html]`. Use rusqlite with `bundled` so the version is controlled; docs.rs shows rusqlite 0.43.0 with `libsqlite3-sys` 0.40.0 (https://docs.rs/rusqlite/latest/rusqlite/) but does not state the bundled SQLite version: assert `sqlite_version() >= 3.37` in a startup test.
- Go does `Receive` inside its own `*sql.DB` handle (queue.go:76 comment: Send cannot join a caller's tx). In Rust, use one writer connection (`Mutex<Connection>` or a dedicated blocking thread) with `BEGIN IMMEDIATE`; WAL + `busy_timeout=5000` + `foreign_keys=ON` to match `database.go:33` (`?_journal=WAL&_timeout=5000&_fk=true`).
- `rusqlite::Connection` is `Send` but not `Sync` `[unverified; docs page did not state]`: do not share via `Arc` without a Mutex; wrap blocking DB calls in `tokio::task::spawn_blocking` (or use `tokio-rusqlite`).
- Rust and Go processes must not run concurrently on the same DB unless both use `BEGIN IMMEDIATE`-style receive; fine under WAL otherwise.
- Postie ordering: `priority desc, created`: keep `created` strftime default so mixed Go/Rust rows sort consistently.
Recommendation: own implementation (~150 lines), verified by a cross-language fixture test: DB written by Go (`goqite.Send`) is received/extended/deleted by Rust and vice versa.

## 2. goose -> migration runner

Go usage: `internal/database/migrations.go`: `goose.SetBaseFS(embed)`, `SetDialect("sqlite3")`, `goose.Up/Down/UpTo/Reset/GetDBVersion`, dir `migrations`. Files `001`..`010` (note: there are TEN, `010_add_web_auth.sql` creates `web_admin`, `web_auth_secret`, `web_sessions`). Also `IsLegacyDatabase()` (goqite exists, no `goose_db_version`) and a destructive `RecreateDatabase()`.

goose bookkeeping (from `github.com/pressly/goose/v3@v3.26.0/internal/dialects/sqlite3.go`, https://github.com/pressly/goose):
```
CREATE TABLE goose_db_version (id INTEGER PRIMARY KEY AUTOINCREMENT, version_id INTEGER NOT NULL, is_applied INTEGER NOT NULL, tstamp TIMESTAMP DEFAULT (datetime('now')));
INSERT INTO goose_db_version (version_id, is_applied) VALUES (?, ?);   -- up: insert (v,1)
SELECT version_id, is_applied FROM goose_db_version ORDER BY id DESC;  -- current state = latest row per version
```
Goose also inserts a `version_id=0, is_applied=1` row at creation. Current version = latest-by-id row semantics (down runs `DELETE FROM goose_db_version WHERE version_id=?`; `SELECT MAX(version_id)` is the version query; the Rust runner only needs up, so: apply versions > max(version_id) where is_applied=1).

Options evaluated:
- `rusqlite_migration`: stores progress in `PRAGMA user_version`, "instead of a migrations table" (https://docs.rs/rusqlite_migration/latest/rusqlite_migration/). Incompatible with goose bookkeeping; would re-run 001-010 on an existing DB. Rejected.
- `refinery` 0.10.0: `V{version}__{name}.sql` files, history in its own table (`refinery_schema_history`) `[unverified name]`; docs do not mention a configurable table (https://docs.rs/refinery/latest/refinery/). Rejected.
- Custom: read the `-- +goose Up` / `-- +goose Down` sections of the same embedded files (`include_dir!` or `rust-embed`), run each Up inside a tx, `INSERT INTO goose_db_version (version_id,is_applied) VALUES (N,1)`. Chosen.
Traps:
- Go-written DBs contain the goose table, so Rust-written DB must too; and a Rust-first DB must still be openable by the Go build during the transition (keep numbering; new Rust-only migrations get 011+ and the Go build will refuse/ignore unknown versions `[unverified: goose errors on missing/unknown versions only with strict mode]`).
- Goose splits statements on `;` unless wrapped in `-- +goose StatementBegin/End`; check 001-010 for triggers (001 notes a trigger removed for exactly this reason). Prefer `execute_batch` on the whole Up section, which SQLite handles natively.
- Reuse one `goose_db_version` creation DDL exactly as above; do not add columns.
- Preserve `IsLegacyDatabase` behavior (goqite present, no goose table => pre-goose DB).

## 3. Wails -> Tauri v2, mux -> axum, gorilla/websocket -> axum ws

Wails today: `main.go` (options/assetserver/mac/windows/linux), bindings in `frontend/src/lib/wailsjs/go/backend/App.{js,d.ts}`, `runtime.EventsEmit` used 29x in internal/, and `frontend/src/lib/api/client.ts` picks `wails` vs `web` by probing `window.go.backend.App` (client.ts:19-23) then calls `App.X()` or `Runtime.EventsOn` (client.ts:599) vs `web-client.ts` (HTTP + WS).

Tauri v2 mapping (https://v2.tauri.app/develop/calling-rust/):
- Go `App` method -> `#[tauri::command]` registered once in `tauri::generate_handler![...]` (calling `invoke_handler` twice keeps only the last). JS: `invoke('cmd', {camelCaseArgs})` from `@tauri-apps/api/core`; args need `Deserialize`, returns `Serialize`; `Err` rejects the promise; errors must be serializable (map to String or custom Serialize). Async commands cannot take borrowed `&str`/`State<'_,T>` params without returning `Result`; use owned types. Shared app state: `.manage(...)` + `tauri::State<T>`.
- Events: Go `runtime.EventsEmit(ctx, name, payload)` -> `AppHandle::emit(name, payload)` (`tauri::Emitter`); JS `listen(name, cb)` from `@tauri-apps/api/event` returns a Promise of an unlisten fn that MUST be awaited before calling (same URL). Events are JSON-only, async, not ordered if handlers are async; use `tauri::ipc::Channel<T>` for ordered high-rate streams (progress). Payload shape differs: Wails `EventsOn` callback gets payload args directly; Tauri hands `{event,id,payload}` => unwrap `.payload` in the adapter.
- Tray/menu: enable cargo feature `tray-icon`; `TrayIconBuilder::new().icon(app.default_window_icon()...).menu(&menu).on_menu_event(..)`; `show_menu_on_left_click(false)`; tray click events unsupported on Linux (https://v2.tauri.app/learn/system-tray/).
- SvelteKit: `@sveltejs/adapter-static` with `fallback: 'index.html'`, root `+layout.ts` `export const ssr = false`, `frontendDist: "../build"`, `devUrl: http://localhost:5173` (https://v2.tauri.app/start/frontend/sveltekit/). Postie's current `svelte.config.js` already uses adapter-static; SPA mode needed because `load` cannot call Tauri APIs during prerender (the layout's `+layout.ts` calls Wails in `browser` only; keep that guard).
- client.ts switch: replace the `wails` branch only. Detect with `'__TAURI_INTERNALS__' in window` `[unverified: v2 injects this global; `withGlobalTauri` was NOT covered by the SvelteKit page]`, or `isTauri()` from `@tauri-apps/api/core` `[unverified]`. Replace `getWailsClient()` with a Proxy that maps `App.GetConfig()` -> `invoke('get_config')`. Fastest path: generate `wailsjs/go/backend/App.js` look-alike shim with the same exported function names so none of the ~55 call sites in client.ts and `types.ts` change; keep `models.ts` types (they are plain classes; fine in a Rust port if serde output stays identical).
- Differences: no `window.go`; Tauri has no Wails `OnStartup/OnDomReady` hooks (use `setup` closure and window events); drag-drop uses Tauri's webview drag-drop events (paths provided) instead of Wails `OnFileDrop` `[unverified]`.

Web mode: gorilla/mux -> `axum` 0.8.9 (docs.rs/axum, https://docs.rs/axum/latest/axum/extract/ws/index.html). ~55 routes in `cmd/web/main.go` (e.g. `HandleFunc("/live", ...).Methods("GET")`, subrouters `/api`, `/api/v1`). Map `Subrouter` -> `Router::nest`, `.Methods("POST")` -> `routing::post`. axum 0.8 path params are `/{id}` (not `/:id`) `[unverified]`. SPA fallback with prerendered pages (main.go:199-222) -> `tower_http::services::ServeDir::fallback(ServeFile index.html)` `[unverified]`; frontend embedded via `rust-embed`/`include_dir` (Go embeds too, main.go:182-190). Auth middleware -> `axum::middleware::from_fn`.

WebSocket (`internal/wshub/hub.go`): gorilla Upgrader with `CheckOrigin` returning true (hub.go:23-28, any origin accepted, keep or tighten), per-client `send` channel (buffer 256), text messages, periodic Ping (hub.go:205-210), `registered`/`session` fields. Rust: `WebSocketUpgrade::on_upgrade`, `socket.split()`; reader task + writer task fed by `tokio::sync::mpsc` (bounded 256, drop client on full like Go) and `broadcast` fan-out. `axum::extract::ws` is built on tokio-tungstenite (optional dep `^0.29.0` on the docs page). `Message::Ping` is available; exact variant types (`Text(Utf8Bytes)`) not confirmed on the page.
Recommendation: share one `postie-core` library (config, queue, services, event enum) used by two thin front-ends, `src-tauri` (commands + `emit`) and `postie-web` (axum + WS hub), mirroring today's `internal/backend` + `cmd/web`. Define an `EventSink` trait so both emit identically.

## 4. Auth

Go (`internal/webauth/service.go`, `internal/apikey/apikey.go`, `cmd/web/auth.go`):
- Passwords: `bcrypt.GenerateFromPassword(pw, bcrypt.DefaultCost)` (Go DefaultCost = 10) stored in `web_admin.password_hash`; length rule 12..72 bytes (Go bcrypt rejects >72).
- Session: HS256 JWT via go-pkgz `token.NewService`, claims = `iss:"postie"`, `sub:username`, `aud:["postie-api"]`, `exp`, `iat`, `jti: random`, plus pkgz field `user:{id,name,...}` (json tag `user`, from `go-pkgz/auth/v2@v2.0.0/token/jwt.go:26-29`; other fields `sess_only`, `handshake`, `no-ava` are omitempty). Validation pins HS256/issuer/audience/exp and then requires a row in `web_sessions(id=jti, expires_at>now)` and `sub == web_admin.username == claims.user.id`. Secret stored as text in `web_auth_secret` and used as RAW bytes (`[]byte(secret)`).
- API key: `crypto/rand` 32 bytes, URL-safe base64 (43 chars, no padding), stored plaintext in SQL store (not hashed): `rand`+`base64::URL_SAFE_NO_PAD`.

Rust:
- `bcrypt` 0.19.3 (https://docs.rs/bcrypt/latest/bcrypt/; source https://github.com/Keats/rust-bcrypt/blob/master/src/lib.rs): `verify` accepts `$2a$`, `$2b$`, `$2x$`, `$2y$`; Go emits `$2a$10$...`, so existing hashes verify. New hashes are `$2b$` and Go's `CompareHashAndPassword` also accepts `$2b$`, so rollback works. `DEFAULT_COST` is 12 in Rust vs 10 in Go: pass cost 10 explicitly to match, or accept 12 (slower login). Use `non_truncating_hash` to reject >72 bytes (it returns `BcryptError::Truncation`), matching Go's rejection; plain `hash` truncation behavior was not stated on the page.
- `jsonwebtoken` 12.0.0 (https://docs.rs/jsonwebtoken/latest/jsonwebtoken/): requires choosing a crypto feature (`aws_lc_rs` or `rust_crypto`; take `rust_crypto` to avoid a C build). `Validation::new(Algorithm::HS256)` + `set_issuer(&["postie"])`, `set_audience(&["postie-api"])`, `set_required_spec_claims(&["exp","iss","aud"])`; `DecodingKey::from_secret(secret.as_bytes())` (raw bytes, NOT base64 variant). Claims struct: `{iss,sub,aud: Vec<String> or String,exp,iat,jti,user:{id,name}}`; Go v5 serializes a single audience as a string, so deserialize `aud` with a string-or-seq helper. Note exp/iat are NumericDate ints (seconds).
- Do NOT port go-pkgz/auth itself (OAuth providers, cookies/XSRF): Postie only uses its `token` package for the claims shape. Keep tokens interoperable so existing sessions survive the cutover (same secret, same jti rows).
- Constant-time compare for API keys: `subtle::ConstantTimeEq` (Go code at apikey.go should be checked; not audited here).

## 5. Other dependencies

### *arr (golift.io/starr v1.3.1; `internal/arr/client.go`)
Postie calls only: `AddNotification` (Sonarr/Radarr/Lidarr/Readarr; body `{name, onDownload:true, onUpgrade:true, implementation:"Webhook", configContract:"WebhookSettings", fields:[{name:"url",value},{name:"method",value:2}]}`), `DeleteNotification(id)`, `GetSystemStatus` (connectivity test). Paths: `/api/v3/notification` + `/api/v3/system/status` for Radarr/Sonarr, `/api/v1/...` for Lidarr/Readarr (starr module cache `{radarr,sonarr,lidarr,readarr}/notification.go`, `system.go`; library overview https://pkg.go.dev/golift.io/starr). No Rust client crate used; auth header is `X-Api-Key` `[unverified: starr uses it, not re-grepped]`. Recommendation: ~80 lines of `reqwest` + `serde_json`. Trap: Radarr/Sonarr add may need extra defaults per version (starr adds none, so parity is simple); keep the Go `notificationName` constant so existing webhooks are recognized.

### Log rotation (lumberjack, `internal/backend/app.go:268`)
Settings: `MaxSize 5MB, MaxBackups 3, MaxAge 28d, Compress = (GOOS != windows)`, JSON slog to stdout+file. `file-rotate` 0.8.0 (https://docs.rs/file-rotate/latest/file_rotate/): `ContentLimit::Bytes`, `AppendCount::new(3)`, `Compression::OnRotate(n)` (gzip, `.gz` suffix); age-based deletion exists only in the timestamp scheme (`FileLimit::Age`), so with count scheme sweep files older than 28d at startup. Traps: Windows compression off for the same lock reason; lumberjack names backups `name-<timestamp>.log`, file-rotate `name.log.1`, so old backups will not be cleaned by the new scheme (acceptable). Wire as a `tracing_subscriber` JSON layer writer via `tracing-appender::non_blocking` `[unverified]`. `tracing-appender`'s built-in rolling is time-based only `[unverified]`, hence file-rotate.

### fsnotify (`internal/watcher/watcher.go`)
`fsnotify` appears only as an indirect dep (go.mod:183). The watcher is a polling scanner (`time.NewTicker(CheckInterval)`, `filepath.WalkDir`, hourly cache cleanup). Port as-is: `tokio::time::interval` + `walkdir`; no `notify` crate needed (and avoids inotify limits on network mounts, a reason polling was probably chosen). Keep `par2_siblings.go` logic.

### brotli / zstd / zip (`internal/nzb/existing.go:66-95`, `nzb.go`, `internal/manifest/manifest.go`)
Crates: `zstd`, `brotli` (rust-lang), `flate2`/`zip`. KEY TRAP: Go passes `config.Level` straight into klauspost `zstd.EncoderLevel(level)`, whose valid range is only 1..4 (`encoder_options.go:221-222` returns "unknown encoder level" for `<= 0` or `>= speedLast`), yet config validation allows 1-22 (`config.go:776`) and defaults to 3. So today levels 5-22 FAIL at NZB write time. In Rust map klauspost 1->zstd 1 (Fastest), 2->3 (Default), 3->7 (Better), 4->11 (Best) `[approximate, per klauspost docs]`, and decide whether to clamp 5-22 (recommended: accept real zstd levels 1-22 directly for new configs; keep 1-4 mapping only if you want byte-compat, which zstd does not guarantee). Decoders are interoperable across implementations. Brotli level 0-11 passes straight through (`brotli::CompressorWriter` takes quality + lgwin; Go default lgwin 22). `manifest.go` writes zstd JSONL streams that Go must still read: zstd frames are standard, so `zstd::stream::Decoder` reads them; check multi-frame/streaming truncation behavior on crash recovery.

### SOCKS5 (`internal/config/config.go:907-935`)
Go: `proxy.SOCKS5("tcp", s.ProxyURL, nil, proxy.Direct)` with `nil` auth and passes the raw `ProxyURL` as the address, so the documented `socks5://user:pass@host:port` form is NOT parsed (the scheme/credentials would break `Dial`): a latent bug. Then dials `addr` through the proxy and wraps TLS using the server's TLS config. Rust: `tokio_socks::tcp::Socks5Stream::connect(proxy_addr, target)` / `connect_with_password(proxy, target, user, pass)` (0.5.3; https://docs.rs/tokio-socks/latest/tokio_socks/tcp/socks5/struct.Socks5Stream.html) then `tokio-rustls` over the stream (`InsecureSkipVerify` => custom `ServerCertVerifier`, ServerName = host). Parse the URL with `url::Url`, supporting `socks5://[user:pass@]host:port`; decide whether `host:port` without scheme (what Go effectively accepts) remains valid: accept both. Domain-name remote resolution is not stated on the docs page `[unverified]`; pass the target as `(host_str, port)` and test against a proxy that logs ATYP.

### YAML (`internal/config/config.go`, yaml.v3)
`serde_yaml_ng` (fork of dtolnay's serde-yaml, "compatible as much as possible", YAML 1.1 only; README example `0.10`; https://github.com/acatton/serde-yaml-ng). Original `serde_yaml` is archived `[unverified; README did not say]`. Compat traps:
- `Duration` is a custom string type (`config.go:60-100`): "5s", "1m30s" Go syntax, empty string allowed => implement a newtype with Go-duration parsing (`humantime` is NOT identical; "1h30m", "300ms" fine but Go also accepts "1.5h", "µs"); serialize as `String`.
- yaml.v3 is YAML 1.2-ish for booleans (only true/false), serde_yaml_ng is 1.1 (`yes/no/on/off` become bools when unquoted) => quote strings like `no` when writing; unquoted `on`/`off` in hand-edited files parse differently between Go and Rust only for string fields.
- Unknown fields: yaml.v3 `Unmarshal` ignores them; use serde without `deny_unknown_fields` to stay lenient. `omitempty` => `#[serde(skip_serializing_if = ...)]`; field names are already snake_case in `yaml:"..."` tags.
- Config rewrite loses comments (Go does too: `yaml.Marshal`); also key order follows struct order, so keep struct order equal to Go.
- Validation/defaults (config.go ~660-790: compression level defaults, ranges) must be ported to behave identically; add a golden test loading `config-example.yaml` and the repo's real configs and round-tripping them.
- Config-change rule from CLAUDE.md still applies: any config field => TS bindings (`models.ts`), settings UI, i18n keys (en/es/fr/tr under `frontend/src/lib/locales/`), `types.ts`; with Tauri, generate TS types from Rust (e.g. `ts-rs`/`specta`) `[unverified]` rather than Wails `generate module`.

## Open items to verify before coding
- `window.__TAURI_INTERNALS__` / `isTauri()` detection and drag-drop event API; `withGlobalTauri` semantics.
- axum 0.8 path syntax and `tower_http` SPA fallback pattern.
- goose behavior when DB has versions the binary does not know (matters for Go<->Rust rollback).
- Bundled SQLite version of rusqlite 0.43 (assert >= 3.37 for STRICT).
