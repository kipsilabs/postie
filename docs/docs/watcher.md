---
sidebar_position: 6
---

# File Watcher

The file watcher is a powerful feature in Postie that allows you to automatically post files as they appear in a designated directory. This is particularly useful for automated workflows where files are downloaded or created and need to be posted to Usenet without manual intervention.

**💡 Tip: The file watcher is easiest to configure and monitor through the web UI, which provides directory selection, pattern testing, and real-time status updates.**

## How It Works

The file watcher scans the configured directory at regular intervals. When a file appears that meets the configured criteria (size, name pattern, etc.), Postie will automatically process and post it to Usenet.

## Configuration

### Using the Web UI (Recommended)

1. Open the web interface at `http://localhost:8080`
2. Navigate to **Settings → Automation**
3. Configure the file watcher with the intuitive interface:
   - Enable/disable the watcher with a toggle
   - Select directories with a file browser
   - Test ignore patterns with real-time validation
   - Set schedules with time pickers
   - Monitor watcher status and activity

### Manual YAML Configuration

Postie supports **multiple file watchers** configured as an array under `watchers`. The legacy single `watcher:` key (used in v1 configs) is still accepted and will be automatically migrated on first load.

```yaml
# Multiple watchers — each entry watches a different directory
watchers:
  - name: "downloads" # Optional label for this watcher instance
    enabled: true # Enable this watcher
    watch_directory: "/path/to/watch" # Directory to monitor
    size_threshold: 104857600 # Minimum size for batch processing (100MB)
    schedule: # Optional posting schedule
      start_time: "00:00" # When to start posting (24h format)
      end_time: "23:59" # When to stop posting (24h format)
    ignore_patterns: # File patterns to ignore
      - "*.tmp"
      - "*.part"
      - "*.!ut"
    min_file_size: 1048576 # Minimum file size to process (1MB)
    check_interval: 5m # How often to check for new files
    delete_original_file: false # Whether to delete source files after successful upload
    single_nzb_per_folder: false # Create one NZB per folder instead of per file (default: false)
    follow_symlinks: false # Follow symbolic links during scanning (default: false)
    min_file_age: 60s # Min time since last modification before a file is eligible (default: 60s; 0s = no wait)
    min_file_age_to_delete: 0s # Min time after upload before deleting source file (default: 0s)
```

To watch multiple directories, add additional entries to the `watchers` array:

```yaml
watchers:
  - name: "tv-shows"
    enabled: true
    watch_directory: "/downloads/tv"
    # ...
  - name: "movies"
    enabled: true
    watch_directory: "/downloads/movies"
    # ...
```

> **Backward compatibility**: The old single-watcher `watcher:` key is still accepted for v1 config files and will be automatically migrated to the `watchers` array on first load.

### Configuration Options

- **name**: Optional label to identify this watcher in logs and the web UI
- **enabled**: Whether this watcher instance is active
- **watch_directory**: The directory to monitor for new files
- **size_threshold**: Minimum accumulated size of files before batch processing
- **schedule**: Optional time window when posting is allowed
  - **start_time**: When to start posting each day (24h format)
  - **end_time**: When to stop posting each day (24h format)
- **ignore_patterns**: File patterns to ignore (uses glob syntax)
- **min_file_size**: Minimum size of individual files to process
- **check_interval**: How frequently to scan the watch directory
- **delete_original_file**: Whether to permanently delete source files after successful upload (default: false)
- **single_nzb_per_folder**: When true, creates one NZB per folder instead of one NZB per file (default: false)
- **follow_symlinks**: Whether to follow symbolic links during directory scanning; set to false (default) to avoid double-counting files outside the watch directory
- **min_file_age**: Minimum time since last modification before a file is eligible for upload — prevents uploading files still being written (default: 60s). `0s` uploads as soon as the file size stops changing between two scans
- **min_file_age_to_delete**: Minimum time after a successful upload before the source file is deleted; useful when a downstream tool needs time to import the NZB (requires `delete_original_file: true`, default: 0s)

## Starting the Watcher

### Using the Web Interface

The file watcher can be started and monitored directly from the web interface. The dashboard shows watcher status, monitored directories, and recent activity.

### Command Line

To start Postie in watch mode from the command line:

```bash
./postie watch -config config.yaml -d /path/to/watch_dir -o ./output
```

If you're using Docker, the watcher will automatically start if you've configured the watcher section in your config file.

## Behavior

1. **File Detection**: Postie scans the watch directory at the interval specified by `check_interval`
2. **File Filtering**: Files are filtered based on `min_file_size` and `ignore_patterns`
3. **Batch Processing**: Files are accumulated until they reach the `size_threshold`
4. **Schedule Checking**: If a schedule is configured, Postie will only post during the allowed time window
5. **Processing**: Files are processed according to your configuration (PAR2, obfuscation, etc.)
6. **Moving**: After successful posting, files are moved to the `output_dir`
7. **Deleting**: The original files are deleted after the nzb are created in the `output_dir`

## Use Cases

The file watcher is particularly useful for:

1. **Automated Pipelines**: Automatically post files as they are downloaded
2. **Scheduled Posting**: Only post during off-peak hours to maximize bandwidth
3. **Batch Processing**: Accumulate small files for more efficient posting

## Tips and Best Practices

1. **Set Appropriate Thresholds**: Adjust `min_file_size` and `size_threshold` based on your typical file sizes
2. **Use Ignore Patterns**: Prevent partial downloads from being processed using patterns like `*.part` or `*.incomplete`
3. **Schedule During Off-Peak Hours**: If your ISP has peak/off-peak hours, schedule posting during off-peak times
4. **Monitor Disk Space**: Ensure both watch and output directories have sufficient space
5. **Logging**: When running in watch mode, consider redirecting output to a log file for monitoring:

```bash
./postie watch -config config.yaml -d /path/to/watch_dir -o ./output > postie.log 2>&1
```
