---
sidebar_position: 4
---

# Configuration

Every setting can be changed in the web UI under **Settings**, which validates input and saves
automatically. This page shows each settings section next to its YAML keys, so you can use either.

Settings has four tabs: **Core Configuration**, **Upload Settings**, **File Processing** and
**Automation**. Some fields only appear with **Advanced Mode** (top right of the Settings page) switched
on; the screenshots below show advanced mode.

## Where the config lives

| How you run Postie | Config file |
|--------------------|-------------|
| Desktop app, `postie-web` on Linux | `~/.config/postie/config.yaml` |
| Desktop app, `postie-web` on macOS | `~/Library/Application Support/Postie/config.yaml` |
| Desktop app, `postie-web` on Windows | `%APPDATA%\Postie\config.yaml` |
| Docker | `/config/config.yaml` |
| `postie` command line | the file passed with `-c` (default `./config.yaml`) |

The database (`postie.db`) sits next to the config file. If you edit the file by hand while the server
runs, restart it to pick up the changes.

### Defaults

Keys you leave out of the file get their default when the config loads, so a config only needs the keys
you want to change. That includes each `watchers` entry (see [File Watcher](./watcher.md#defaults)).
Postie never writes those defaults back into your file, so a hand-written config stays as you wrote it.

Durations are written like `30s`, `5m` or `1h`. Sizes are in bytes.

## Core Configuration

### General

![General settings](/img/settings/general.png)

```yaml
output_dir: ./output # where NZB files are written
maintain_original_extension: true # see File naming below
```

`output_dir` applies to uploads from the UI, the API and the watchers. The `postie` command line uses `-o`
instead.

### Servers

![Upload pool](/img/settings/servers-upload.png)

```yaml
servers:
  - name: Primary provider # optional label shown in the UI
    host: news.example.com
    port: 563 # usually 119 plain, 563 TLS
    username: user
    password: pass
    ssl: true
    insecure_ssl: false # skip certificate validation
    max_connections: 20
    inflight: 10 # requests in flight per connection (0 = 10)
    max_connection_idle_time_in_seconds: 300 # 0 = 300
    max_connection_ttl_in_seconds: 3600 # 0 = 3600
    proxy_url: "" # socks5://user:pass@host:1080
    enabled: true
    role: upload # upload or verify
```

| Key | Default | Notes |
|-----|---------|-------|
| `host`, `port`, `max_connections` | | Required. `max_connections` must be positive. |
| `role` | `upload` | `upload` servers post articles. `verify` servers only check that articles arrived. |
| `enabled` | `true` | Disabled servers are ignored. |
| `inflight` | `10` | Pipelined requests per connection. |
| `proxy_url` | | Optional SOCKS5 proxy. |

You need at least one enabled upload server. Each server must have a different host and username
combination. Articles are spread round-robin across all enabled upload servers, so mixing providers on
different backbones means a post is split across them.

#### Verify servers

![Verify pool](/img/settings/servers-verify.png)

Add servers from other providers with `role: verify` to check that articles reached their backbone.
Without a verify server, checks run on the upload servers. Configs from v1 that used `check_only: true` are
migrated to `role: verify` automatically.

## Upload Settings

### Posting

![Posting configuration](/img/settings/posting.png)

```yaml
posting:
  max_retries: 3
  retry_delay: 5s
  article_size_in_bytes: 768000
  throttle_rate: 0 # bytes per second, 0 = unlimited
  upload_buffer_memory_limit: 0 # 0 = auto
  obfuscation_policy: full # full, partial, none
  par2_obfuscation_policy: full # full, partial, none
  message_id_format: random # random or nxg
  group_policy: each_file # each_file or all
  wait_for_par2: false
```

| Key | Default | Notes |
|-----|---------|-------|
| `max_retries`, `retry_delay` | `3`, `5s` | Retries for a failed article post. |
| `article_size_in_bytes` | `768000` | New configs get 768000; a config without the key uses 750000. |
| `throttle_rate` | `0` | Aggregate upload limit across all connections, in bytes per second. The UI shows it in MB/s. |
| `upload_buffer_memory_limit` | `0` | Memory for in-flight article buffers. `0` sizes it automatically between 64 MiB and 512 MiB. |
| `obfuscation_policy` | `full` | See [Obfuscation](./obfuscation.md). |
| `par2_obfuscation_policy` | `full` | The same, for PAR2 files. |
| `message_id_format` | `random` | `random` or [`nxg`](https://github.com/javi11/nxg) (deterministic IDs). |
| `group_policy` | `each_file` | `each_file` posts each file to one randomly chosen group; `all` cross-posts to every group. |
| `wait_for_par2` | `false` | `false` creates PAR2 while the file uploads. `true` creates PAR2 first. With PAR2 off it is always `true`. |

With `wait_for_par2: false`, one upload slot and room on the temp disk, Postie also creates the *next*
queued job's PAR2 while the current one uploads, so only the first job in a queue waits for parity. If
PAR2 creation fails, the file is posted without PAR2.

#### Headers

![Post headers](/img/settings/post-headers.png)

```yaml
posting:
  post_headers:
    add_nxg_header: false # adds X-NXG (not with full obfuscation or nxg message IDs)
    default_from: "" # poster address used for every article, e.g. poster@example.com
    custom_headers:
      - name: X-Custom-Header
        value: custom-value
```

`default_from` must be a valid email address. When set, it replaces the random poster under every
obfuscation policy.

#### Newsgroups

![Newsgroups](/img/settings/newsgroups.png)

```yaml
posting:
  groups:
    - name: alt.binaries.test
      enabled: true
```

At least one group is required. Disabled groups are skipped.

### Post check

![Post check configuration](/img/settings/post-check.png)

After posting, Postie asks the servers (`STAT`) whether each article exists and reposts the missing ones.

```yaml
post_check:
  enabled: true
  delay: 10s # wait before checking an article
  max_reposts: 1
  check_during_upload: true
  check_connections: 0 # 0 = auto
  max_concurrent_checks: 0 # 0 = auto
  stat_batch_size: 100
```

| Key | Default | Notes |
|-----|---------|-------|
| `enabled` | `true` | |
| `delay` | `10s` | Time between posting an article and checking it. |
| `max_reposts` | `1` | Reposts per missing article. |
| `check_during_upload` | `true` | Check articles shortly after they are posted, while the rest of the upload runs. |
| `check_connections` | `0` | Upload connections set aside for those checks when there is no verify server. `0` picks a number automatically. |
| `max_concurrent_checks` | `0` | Parallel checks. `0` sizes it automatically. |
| `stat_batch_size` | `100` | Articles per batch of pipelined checks. |

#### Deferred verification

![Deferred verification](/img/settings/post-check-deferred.png)

Articles that are not visible yet are checked again later, with exponential backoff:

```yaml
post_check:
  deferred_check_delay: 30s # first re-check
  deferred_max_retries: 5
  deferred_max_backoff: 5m
  deferred_check_interval: 2m # how often the deferred worker runs
  deferred_batch_size: 10000 # articles per run
```

### Queue

![Queue configuration](/img/settings/queue.png)

```yaml
queue:
  max_concurrent_uploads: 1
  min_size_to_start: 0 # bytes; 0 = start immediately
```

`max_concurrent_uploads` is how many queued jobs upload at once. `min_size_to_start` holds the queue back
until the pending jobs add up to at least that many bytes.

### Connection pool

Connections open on demand and close after the server's `max_connection_idle_time_in_seconds`, so there are
no pool settings. Older configs may still contain a `connection_pool` section with `min_connections` and
`health_check_interval`; it is accepted and ignored.

## File Processing

### File naming

![File naming](/img/settings/file-naming.png)

```yaml
maintain_original_extension: true
```

`true` names the NZB after the full file name (`movie.mkv.nzb`); `false` drops the extension
(`movie.nzb`).

### PAR2

![PAR2 recovery files](/img/settings/par2.png)

Postie creates PAR2 recovery files itself; no external tool is needed. The files work with any PAR2 client
(par2cmdline, MultiPar, QuickPar).

```yaml
par2:
  enabled: true
  redundancy: "5%"
  temp_dir: /tmp # new configs use the system temp folder; empty = next to the source files
  maintain_par2_files: false
  skip_if_par2_exists: true
  max_concurrent_jobs: 1
  memory_limit: 4294967296 # 4 GiB
  slice_size: 10485760 # 10 MiB
  gf16_method: auto
  parpar_binary_path: "" # use an external ParPar instead of the built-in creator
  parpar_extra_args: []
```

| Key | Default | Notes |
|-----|---------|-------|
| `redundancy` | `5%` | A percentage (`10%` or `10`) or a ParPar expression such as `1n*1.2`. |
| `temp_dir` | system temp | Sets are created in `<temp_dir>/postie-par2/`; orphaned sets older than 24 hours are swept every hour. Empty writes PAR2 files next to the source files. |
| `maintain_par2_files` | `false` | `true` writes the PAR2 files next to the NZB and keeps them. |
| `skip_if_par2_exists` | `true` | Folder uploads that already contain PAR2 files get no new set. |
| `max_concurrent_jobs` | `1` | PAR2 jobs running at once. |
| `memory_limit` | 4 GiB | Above this, encoding runs in several passes over the input. |
| `slice_size` | 10 MiB | PAR2 slice (block) size. |
| `num_goroutines` | `0` | Worker threads for the built-in creator. `0` picks a number from the CPU count. |
| `gf16_method` | `auto` | Pins the SIMD kernel. Leave on `auto`: a kernel the CPU lacks makes PAR2 creation fail. |
| `parpar_binary_path` | | Path to a ParPar binary to shell out to, with `parpar_extra_args` appended. |

`gf16_method` accepts `auto`, `lookup`, `lookup3`, `shuffle-avx2`, `shuffle-avx512`, `shuffle-vbmi`,
`xor-jit-avx2`, `affine-avx2`, `affine-avx512`, `shuffle-neon` and `clmul-neon`.

### Duplicate lookup

![Duplicate lookup](/img/settings/duplicate-lookup.png)

```yaml
duplicate_lookup:
  enabled: false
  url: https://lookup.example/api/duplicate-lookup
  api_key: ""
  timeout: 60s # at most 5m
```

Before uploading a file, Postie can ask an HTTP service whether an NZB for it already exists and reuse
that NZB instead. See [Duplicate lookup](./duplicate-lookup.md) for the behaviour and the protocol.

### NZB compression

![NZB compression](/img/settings/nzb-compression.png)

```yaml
nzb_compression:
  enabled: false
  type: zstd # none, zstd, brotli or zip
  level: 3 # zstd 1-22 (default 3), brotli 0-11 (default 4), zip 0-9 (default 6)
```

Compressed NZBs get `.nzb.zst`, `.nzb.br` or `.nzb.zip`. A level of `0` (or no level) uses the type's
default.

## Automation

### File watcher

![File watcher](/img/settings/watcher.png)

```yaml
watchers:
  - name: downloads
    enabled: true
    watch_directory: /data/watch
    check_interval: 5m
    min_file_age: 60s
    min_file_age_to_delete: 0s
    size_threshold: 104857600 # 100 MB
    min_file_size: 1048576 # 1 MB
    delete_original_file: false
    single_nzb_per_folder: false
    follow_symlinks: false
    schedule:
      start_time: "00:00"
      end_time: "23:59"
    ignore_patterns: ["*.tmp", "*.part", "*.!ut"]
```

See [File Watcher](./watcher.md) for what each key does and how omitted keys behave.

### Post-upload script

![Post-upload script](/img/settings/post-upload-script.png)

```yaml
post_upload_script:
  enabled: false
  command: "" # placeholders: {nzb_path}, {source_path}, {source_dir}
  timeout: 30s
  max_retries: 3 # 0 = retry until max_retry_duration
  retry_delay: 30s # doubles after each failure
  max_backoff: 1h
  max_retry_duration: 24h
  retry_check_interval: 1m
```

The command runs after each completed upload. `{nzb_path}`, `{source_path}` and `{source_dir}` are
replaced with the NZB, the uploaded file and its folder; the values are also in the environment as
`POSTIE_NZB_PATH`, `POSTIE_SOURCE_PATH` and `POSTIE_SOURCE_DIR`. On Linux and macOS the placeholders are
passed through those variables, so file names with spaces or quotes are safe. A failed run is retried with exponential backoff until it
succeeds, `max_retries` is used up or `max_retry_duration` passes.

```yaml
# notify a webhook
command: 'curl -X POST -H "Content-Type: application/json" -d "{\"nzb_path\": \"{nzb_path}\"}" https://example.com/notify'
# run a PowerShell script on Windows
command: 'powershell C:\Postie\Scripts\Zip.ps1 {nzb_path}'
```

### External API

![External HTTP API](/img/settings/api.png)

```yaml
api:
  enabled: false
```

Lets other tools queue uploads over HTTP with an API key. See [API and Arr](./integrations.md#http-api).

### Arr integration

![Arr integration](/img/settings/arr.png)

Connects Sonarr and Radarr, so files they import are uploaded automatically. Instances are added in the
UI. See [API and Arr](./integrations.md#sonarr-and-radarr).

## Database

```yaml
database:
  database_type: sqlite # only sqlite is supported
  database_path: ./postie.db
```

The queue and upload history live in this SQLite database. The web server and desktop app keep it next to
`config.yaml`.

## Command line

```text
postie [flags]          upload the files in --dir (or the single --input-file), then exit
postie watch [flags]    keep watching --dir and upload new files

-c, --config <path>        configuration file (default "config.yaml")
-d, --dir <path>           directory to upload or watch (default ".")
-i, --input-file <path>    upload this single file instead of --dir
-o, --output-dir <path>    directory for the NZB files (default ".")
    --ignore-pattern <glob>  skip matching files (repeatable; not with watch)
-v, --verbose              verbose logging
-h, --help                 show help
    --version              show the version
```

```bash
postie -c config.yaml -i ./movie.mkv -o ./nzb
postie -c config.yaml -d ./upload -o ./nzb --ignore-pattern '*.tmp' --ignore-pattern '*.part'
postie watch -c config.yaml -d ./watch -o ./nzb
```

- Without `-i`, every file under `--dir` is uploaded, and `--dir` defaults to the current directory.
- `--ignore-pattern` globs match file names (not directories) at every depth and support `*`, `?` and
  `[0-9]`. Quote them so the shell does not expand them. If nothing is left, Postie reports `no files to
  post` without connecting.
- `postie watch` watches `--dir` (not `watch_directory`) with the settings of the first `watchers` entry,
  and writes NZBs to `--output-dir`.

## Complete example

```yaml
output_dir: ./output
maintain_original_extension: true

servers:
  - name: Primary provider
    host: news.example.com
    port: 563
    username: user
    password: pass
    ssl: true
    max_connections: 20
    role: upload
  - name: Backbone check
    host: news.other-provider.example
    port: 563
    username: user
    password: pass
    ssl: true
    max_connections: 4
    role: verify

posting:
  max_retries: 3
  retry_delay: 5s
  article_size_in_bytes: 768000
  throttle_rate: 0
  upload_buffer_memory_limit: 0
  obfuscation_policy: full
  par2_obfuscation_policy: full
  message_id_format: random
  group_policy: each_file
  wait_for_par2: false
  groups:
    - name: alt.binaries.test
      enabled: true
  post_headers:
    add_nxg_header: false
    default_from: ""
    custom_headers: []

post_check:
  enabled: true
  delay: 10s
  max_reposts: 1
  check_during_upload: true
  check_connections: 0
  max_concurrent_checks: 0
  stat_batch_size: 100
  deferred_check_delay: 30s
  deferred_max_retries: 5
  deferred_max_backoff: 5m
  deferred_check_interval: 2m
  deferred_batch_size: 10000

queue:
  max_concurrent_uploads: 1
  min_size_to_start: 0

par2:
  enabled: true
  redundancy: "5%"
  temp_dir: /tmp
  maintain_par2_files: false
  skip_if_par2_exists: true
  max_concurrent_jobs: 1
  memory_limit: 4294967296
  slice_size: 10485760
  gf16_method: auto
  parpar_binary_path: ""
  parpar_extra_args: []

duplicate_lookup:
  enabled: false
  url: ""
  api_key: ""
  timeout: 60s

nzb_compression:
  enabled: false
  type: none
  level: 0

watchers:
  - name: downloads
    enabled: false
    watch_directory: ""
    check_interval: 5m
    min_file_age: 60s
    min_file_age_to_delete: 0s
    size_threshold: 104857600
    min_file_size: 1048576
    delete_original_file: false
    single_nzb_per_folder: false
    follow_symlinks: false
    schedule:
      start_time: "00:00"
      end_time: "23:59"
    ignore_patterns: ["*.tmp", "*.part", "*.!ut"]

post_upload_script:
  enabled: false
  command: ""
  timeout: 30s
  max_retries: 3
  retry_delay: 30s
  max_backoff: 1h
  max_retry_duration: 24h
  retry_check_interval: 1m

api:
  enabled: false

database:
  database_type: sqlite
  database_path: ./postie.db
```
