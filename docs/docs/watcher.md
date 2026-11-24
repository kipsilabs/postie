---
sidebar_position: 6
---

# File Watcher

A watcher scans a directory on an interval and queues the files that appear in it, so downloads or other
tools can drop files in and get NZBs out without manual steps. You can run several watchers, one per
directory.

## Setting it up in the UI

Open **Settings → Automation → File Watcher**, click **Add Watch Directory**, and set the directory and
filters. The watcher starts as soon as it is enabled; there is no separate start step. In Docker, use a
path inside the container, such as `/watch`.

![File watcher settings](/img/settings/watcher.png)

## YAML

```yaml
watchers:
  - name: tv
    enabled: true
    watch_directory: /downloads/tv
    check_interval: 5m
    min_file_age: 60s
    size_threshold: 104857600 # 100 MB
    min_file_size: 1048576 # 1 MB
    ignore_patterns: ["*.tmp", "*.part", "*.!ut"]
    schedule:
      start_time: "00:00"
      end_time: "23:59"
    delete_original_file: false
    min_file_age_to_delete: 0s
    single_nzb_per_folder: false
    follow_symlinks: false
  - name: movies
    enabled: true
    watch_directory: /downloads/movies
    # ...
```

The single `watcher:` key from v1 configs is still read and migrated to `watchers`.

## Options

| Key | Meaning |
|-----|---------|
| `name` | Label shown in the UI and logs. |
| `enabled` | Whether this watcher runs. |
| `watch_directory` | Directory to scan, including subdirectories. |
| `check_interval` | Time between scans. |
| `min_file_age` | A file must be unmodified for this long, and its size unchanged since the previous scan, before it is queued. `0s` only requires the unchanged size. |
| `size_threshold` | A scan does nothing until the files in the directory add up to at least this many bytes. `0` disables it. |
| `min_file_size` | Smaller files are skipped. |
| `ignore_patterns` | Glob patterns matched against file names (`*`, `?`, `[0-9]`). |
| `schedule` | Daily window (24-hour `HH:MM`) when scans run. Empty start or end means always. |
| `delete_original_file` | Delete the source files after a successful upload. Files whose NZB came from [duplicate lookup](./duplicate-lookup.md) are always kept. |
| `min_file_age_to_delete` | Wait this long after the upload before deleting, for example so another tool can import the NZB first. Needs `delete_original_file: true`. |
| `single_nzb_per_folder` | One NZB per folder instead of one per file. |
| `follow_symlinks` | Follow symbolic links while scanning. Off by default, so files outside the directory are not picked up twice. |

## Defaults {#defaults}

A watcher added in the UI, or the one in a brand-new config, starts with the defaults in the left column.
A watcher entry written by hand in YAML does **not** get those defaults for keys it leaves out:

| Key | New watcher (UI) | Key left out of a YAML entry |
|-----|------------------|------------------------------|
| `check_interval` | `5m` | `60s` |
| `min_file_age` | `60s` | `60s` |
| `size_threshold` | 100 MB | `0` (no threshold) |
| `min_file_size` | 1 MB | `0` (no minimum) |
| `ignore_patterns` | `*.tmp`, `*.part`, `*.!ut` | none |
| `schedule` | `00:00`–`23:59` | always |
| `min_file_age_to_delete` | `0s` | `0s` |

Write every key you care about when you edit a watcher by hand.

## What happens to a file

1. Each scan (inside the schedule) lists the directory, skips ignored and too-small files, and checks the
   size threshold.
2. A file is queued once it is old enough and its size has stopped changing.
3. The queue uploads it like any other job: PAR2, obfuscation, posting and verification follow your
   settings.
4. The NZB is written to the global `output_dir`, keeping the file's path relative to the watch directory.
5. The source file stays where it is, unless `delete_original_file` is on.

The dashboard shows queued and running watcher jobs. **Scan Now** on the dashboard triggers a scan
without waiting for the interval.

## Command line

```bash
postie watch -c config.yaml -d /path/to/watch -o ./nzb
```

`postie watch` scans the directory given with `-d` (default: the current directory) using the settings of
the **first** `watchers` entry, and writes NZBs to `-o` (default: the current directory). It runs until you
stop it. To log to a file:

```bash
postie watch -c config.yaml -d /path/to/watch -o ./nzb > postie.log 2>&1
```

## Tips

- Use `min_file_age` and `ignore_patterns` together so partial downloads are never uploaded.
- Put `schedule` to work if your connection has off-peak hours.
- Keep an eye on free space on the watch, output and PAR2 temp disks.
