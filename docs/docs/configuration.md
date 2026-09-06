---
sidebar_position: 4
---

# Configuration Guide

This guide explains all the configuration options available in Postie. **We strongly recommend using the web UI to configure Postie**, as it provides a user-friendly interface with validation, real-time feedback, and organized sections for all configuration options.

## Using the Web UI (Recommended)

The easiest way to configure Postie is through the web interface:

1. Start Postie (using Docker or the binary)
2. Open your browser and navigate to `http://localhost:8080` (or your configured port)
3. Click on the "Settings" tab
4. Configure all options through the intuitive interface with organized tabs:
   - **Core Configuration**: General settings and server configuration
   - **Upload Settings**: Posting configuration, connection pool, and post-check settings
   - **File Processing**: PAR2 and NZB compression settings
   - **Automation**: File watcher and post-upload script configuration

The web UI automatically validates your configuration, provides helpful descriptions for each option, and saves changes instantly. It also shows your current configuration status and any errors that need to be resolved.

## Manual YAML Configuration

If you prefer to manually edit the YAML configuration file, this section explains all available options.

## Complete Configuration Example

Below is a complete example of a configuration file with all available options:

```yaml
# Global output directory for processed files and NZB files
output_dir: "./output"

# Whether to maintain the original file extension in the NZB filename (default: true)
maintain_original_extension: true

servers:
  - host: news.example.com
    port: 119
    username: user
    password: pass
    ssl: true
    max_connections: 10
    max_connection_idle_time_in_seconds: 300
    max_connection_ttl_in_seconds: 3600
    insecure_ssl: false
    enabled: true
    role: upload # "upload" (default) or "verify" — see Server Roles below
    inflight: 10 # Concurrent requests per connection (default: 10)
    # proxy_url: socks5://user:pass@host:1080 # Optional SOCKS5 proxy

connection_pool:
  min_connections: 5
  health_check_interval: 1m

posting:
  # Wait for par2 generation before uploading the file.
  # Setting this to false could improve the posting speed but you risk into posting
  # a something without par2 if this fails, also it will increase the resource usage
  wait_for_par2: true
  max_retries: 3
  retry_delay: 5s
  article_size_in_bytes: 750000
  groups:
    - name: alt.binaries.test
      enabled: true
  throttle_rate: 0 # unlimited (bytes per second)
  message_id_format: random # Options: random, nxg
  obfuscation_policy: full # Options: full, partial, none
  par2_obfuscation_policy: full # Options: full, partial, none
  group_policy: each_file # Options: all, each_file (default: each_file)
  post_headers:
    add_nxg_header: false
    default_from: ""
    custom_headers:
      - name: "X-Custom-Header"
        value: "custom-value"

post_check:
  enabled: true
  delay: 10s
  max_reposts: 1
  deferred_check_delay: 30s # Delay before first deferred re-check (default: 30s)
  deferred_max_retries: 5 # Max deferred retry attempts (default: 5)
  deferred_max_backoff: 5m # Max backoff cap for deferred checks (default: 5m)
  deferred_check_interval: 2m # Worker poll interval for deferred checks (default: 2m)
  deferred_batch_size: 10000 # Articles processed per deferred check cycle (default: 10000, sized to sweep a full NZB)

par2:
  enabled: true
  redundancy: "1n*1.2" # ParPar redundancy expression (default: "1n*1.2"); percentage format also accepted (e.g. "10%")
  temp_dir: "" # Optional temporary directory for PAR2 operations
  maintain_par2_files: false # Keep PAR2 files after successful upload
  parpar_binary_path: "" # Path to external parpar binary (empty = use built-in)
  gf16_method: auto # GF16 SIMD kernel for the built-in generator. Leave on auto unless troubleshooting; a kernel the CPU lacks makes PAR2 creation fail. Options: auto, lookup, lookup3, shuffle-avx2, shuffle-avx512, shuffle-vbmi, xor-jit-avx2, affine-avx2, affine-avx512, shuffle-neon, clmul-neon

nzb_compression:
  enabled: false # Whether to enable compression of the output NZB file
  type: none # Options: none, zstd, brotli, zip
  level: 0 # Compression level (zstd: 1-22, brotli: 0-11, zip: 0-9)

# Multiple watchers are supported. Use the watchers array (v1 single watcher key is still accepted for backward compatibility).
watchers:
  - name: "main" # Optional label for this watcher
    enabled: false # Whether to enable the file watcher
    watch_directory: "" # Directory to watch for new files
    size_threshold: 104857600 # 100MB
    schedule:
      start_time: "00:00"
      end_time: "23:59"
    ignore_patterns:
      - "*.tmp"
      - "*.part"
      - "*.!ut"
    min_file_size: 1048576 # 1MB
    check_interval: 5m
    delete_original_file: false # Delete source files after successful upload
    single_nzb_per_folder: false # Create one NZB per folder instead of per file (default: false)
    follow_symlinks: false # Follow symbolic links during directory scanning (default: false)
    min_file_age: 60s # Min time since last modification before processing (default: 60s)
    min_file_age_to_delete: 0s # Min time after upload before deleting source file (default: 0s)

# Database configuration (used for queue persistence)
database:
  database_type: sqlite # Options: sqlite, postgres, mysql (default: sqlite)
  database_path: ./postie.db # Database file path or connection string (default: ./postie.db)

# Queue configuration for upload management
queue:
  max_concurrent_uploads: 1 # Maximum concurrent uploads from queue

# Post upload script configuration
post_upload_script:
  enabled: false # Whether to enable post upload script execution
  command: "" # Command to execute, use {nzb_path} placeholder for NZB file path
  timeout: 30s # Maximum time to wait for command execution
  max_retries: 3 # Max retry attempts for failed script executions (0 = unlimited, default: 3)
  retry_delay: 30s # Base delay between retries with exponential backoff (default: 30s)
  max_backoff: 1h # Maximum backoff cap for retries (default: 1h)
  max_retry_duration: 24h # Total max window to keep retrying (default: 24h)
  retry_check_interval: 1m # How often to check for pending retries (default: 1m)
```

## Configuration Sections

### Usenet Servers

Configure one or more Usenet providers:

```yaml
servers:
  - name: "" # Optional display name shown in the UI instead of "Provider N"
    host: news.example.com # Server hostname
    port: 119 # Server port (typically 119 for non-SSL, 563 for SSL)
    username: user # Your username
    password: pass # Your password
    ssl: true # Whether to use SSL/TLS
    max_connections: 10 # Maximum concurrent connections to this server
    max_connection_idle_time_in_seconds: 300 # Max idle time before recycling a connection
    max_connection_ttl_in_seconds: 3600 # Max total lifetime of a connection
    insecure_ssl: false # Set to true to skip SSL certificate verification
    enabled: true # Whether this server is enabled
    role: upload # "upload" (default) or "verify" — see Server Roles below
    inflight: 10 # Concurrent in-flight requests per connection (default: 10)
    proxy_url: "" # Optional SOCKS5 proxy (format: socks5://user:pass@host:port)
```

You can add multiple servers for redundancy. Postie will automatically fail over to another server if one becomes unavailable. It will also randomly post to any of the servers specified, so be aware if you use different backbones.

#### Server Roles

Each server has a `role` that determines how it is used:

- **`upload`** (default): Used for posting articles. All upload-role servers share the same provider pool.
- **`verify`**: Used only for STAT checks (post verification). Verify servers are never used for posting.

Use `verify` servers when you have access to a second provider whose retention you want to check against, without actually posting to it.

> **Note:** The deprecated `check_only` field from v1 configs is automatically migrated to `role: verify` on first load.

**💡 Tip: Use the web UI to easily add, remove, and test server configurations with real-time validation.**

### Connection Pool

Configure how connections are managed:

```yaml
connection_pool:
  min_connections: 5 # Minimum number of connections to maintain alive in watch mode.
  health_check_interval: 1m # How often to check connection health.
```

**💡 Tip: The web UI provides real-time connection status and allows you to test your configuration before saving.**

### Posting Settings

Configure how files are posted:

```yaml
posting:
  wait_for_par2: true # Wait for PAR2 generation before uploading
  max_retries: 3 # Maximum retry attempts for posting
  retry_delay: 5s # Delay between retry attempts
  article_size_in_bytes: 750000 # Size of each article (default: 750KB)
  groups: # Newsgroups to post to (array of objects with name + enabled)
    - name: alt.binaries.test
      enabled: true
  throttle_rate: 0 # Upload speed limit in bytes/sec (0 = unlimited)
  message_id_format: random # Format of message IDs ("random" or "[nxg](https://github.com/javi11/nxg)")
  obfuscation_policy: full # Level of obfuscation ("full", "partial", or "none")
  par2_obfuscation_policy: full # Obfuscation for PAR2 files
  group_policy: each_file # How to distribute posts ("all" or "each_file") — default: each_file
  post_headers: # Additional headers configuration
    add_nxg_header: false # Whether to add [X-NXG](https://github.com/javi11/nxg) header
    default_from: "" # Default poster name
    custom_headers: # Custom headers to add to the post
      - name: "X-Custom-Header"
        value: "value"
```

**💡 Tip: The web UI provides helpful tooltips and validation for all posting options, making it easy to understand the impact of each setting.**

#### Obfuscation Policies

- **full**: Maximum obfuscation (randomized filenames, subjects, dates, and poster)
- **partial**: Medium obfuscation (obfuscated filenames and subjects, consistent poster)
- **none**: No obfuscation

#### Group Policies

- **all**: Post to all specified groups simultaneously
- **each_file**: Post each file to a different group from the list

### Post Verification

Configure post verification:

```yaml
post_check:
  enabled: true # Whether to verify posts after uploading using STAT method
  delay: 10s # Delay between check attempts (default: 10s)
  max_reposts: 1 # Maximum number of reposts if check fails
  # Deferred check settings — used when an article is not yet available at check time
  deferred_check_delay: 30s # Initial delay before first deferred re-check (default: 30s)
  deferred_max_retries: 5 # Maximum deferred retry attempts (default: 5)
  deferred_max_backoff: 5m # Maximum backoff cap for deferred checks (default: 5m)
  deferred_check_interval: 2m # Worker poll interval for deferred checks (default: 2m)
  deferred_batch_size: 10000 # Articles processed per deferred check cycle (default: 10000, sized to sweep a full NZB)
```

### PAR2 Recovery Files

Postie includes a built-in PAR2 creator — no external binaries are required. PAR2 recovery files are generated natively in Go, producing output compatible with standard PAR2 repair tools (par2repair, MultiPar).

Configure PAR2 recovery file generation:

```yaml
par2:
  enabled: true
  redundancy: "1n*1.2" # ParPar redundancy expression (default: "1n*1.2"); percentage also accepted (e.g. "10%")
  temp_dir: "" # Optional temporary directory for PAR2 operations
  maintain_par2_files: false # Keep PAR2 files after successful upload
  parpar_binary_path: "" # Path to external parpar binary (empty = use built-in)
  gf16_method: auto # GF16 SIMD kernel for the built-in generator. Leave on auto unless troubleshooting; a kernel the CPU lacks makes PAR2 creation fail. Options: auto, lookup, lookup3, shuffle-avx2, shuffle-avx512, shuffle-vbmi, xor-jit-avx2, affine-avx2, affine-avx512, shuffle-neon, clmul-neon
```

**💡 Tip: The web UI provides easy-to-use controls for redundancy settings with preset buttons for common percentages.**

### NZB Compression

Configure NZB file compression:

```yaml
nzb_compression:
  enabled: false # Whether to enable compression of the output NZB file
  type: none # Compression algorithm to use (options: none, zstd, brotli, zip)
  level: 0 # Compression level (zstd: 1-22, brotli: 0-11, zip: 0-9)
```

When compression is enabled, the generated NZB files will be compressed using the specified algorithm and will have the appropriate file extension added (`.nzb.zst` for zstd, `.nzb.br` for brotli, or `.nzb.zip` for zip). This can significantly reduce the size of NZB files, especially for large uploads with many segments.

#### Compression Types

- **none**: No compression (default)
- **zstd**: [Zstandard compression](https://github.com/facebook/zstd) - fast compression with good ratios
- **brotli**: [Brotli compression](https://github.com/google/brotli) - higher compression ratios but slower
- **zip**: Standard ZIP compression - universal compatibility with moderate compression

#### Compression Levels

- **zstd**: 1-22 (higher = better compression but slower, default: 3)
- **brotli**: 0-11 (higher = better compression but slower, default: 4)
- **zip**: 0-9 (higher = better compression but slower, default: 6)

**💡 Tip: The web UI provides compression size estimates and helps you choose the optimal settings for your use case.**

### File Watcher

Postie supports **multiple file watchers** — each watches a different directory. Configure them as an array under `watchers`. The legacy single `watcher:` key (used in v1 configs) is still accepted for backward compatibility and will be automatically migrated.

```yaml
watchers:
  - name: "downloads" # Optional label for this watcher instance
    enabled: false # Whether to enable this watcher
    watch_directory: "" # Directory to watch for new files
    size_threshold: 104857600 # 100MB
    schedule:
      start_time: "00:00" # When to start posting (24h format)
      end_time: "23:59" # When to stop posting (24h format)
    ignore_patterns:
      - "*.tmp"
      - "*.part"
      - "*.!ut"
    min_file_size: 1048576 # 1MB
    check_interval: 5m # How often to check for new files
    delete_original_file: false # Delete source files after upload
    single_nzb_per_folder: false # Create one NZB per folder instead of per file (default: false)
    follow_symlinks: false # Follow symbolic links during scanning (default: false)
    min_file_age: 60s # Min time since last modification before processing (default: 60s)
    min_file_age_to_delete: 0s # Min time after upload before deleting source (default: 0s; requires delete_original_file: true)
```

You can add as many entries as needed under `watchers` to monitor multiple directories simultaneously.

**💡 Tip: The web UI provides an easy way to select directories, test ignore patterns, and validate schedule configurations.**

### Database

Configure the database used for queue persistence:

```yaml
database:
  database_type: sqlite # Database type: sqlite (default), postgres, mysql
  database_path: ./postie.db # File path (sqlite) or connection string (postgres/mysql)
```

### Queue Management

Configure the upload queue system:

```yaml
queue:
  max_concurrent_uploads: 1 # Maximum concurrent uploads from queue (default: 1)
```

### Post Upload Script

Configure commands to run after successful uploads:

```yaml
post_upload_script:
  enabled: false # Whether to enable post upload script execution
  command: "" # Command to execute, use {nzb_path} placeholder for NZB file path
  timeout: 30s # Maximum time to wait for command execution (default: 30s)
  max_retries: 3 # Max retry attempts for failed executions; 0 = unlimited (default: 3)
  retry_delay: 30s # Base delay between retries with exponential backoff (default: 30s)
  max_backoff: 1h # Maximum backoff cap to prevent very long waits (default: 1h)
  max_retry_duration: 24h # Total max window to keep retrying; after this, marked failed (default: 24h)
  retry_check_interval: 1m # How often to check for pending retries (default: 1m)
```

Example webhook notification:

```yaml
post_upload_script:
  enabled: true
  command: 'curl -X POST -H "Content-Type: application/json" -d "{\"nzb_path\": \"{nzb_path}\"}" https://your-webhook-url.com/notify'
  timeout: 30s
```

Example running a PowerShell script on Windows:

```yaml
post_upload_script:
  enabled: true
  command: 'powershell C:\Usenet_Programms\Postie\Scripts\ZIP.ps1 {nzb_path}'
  timeout: 30s
```

**💡 Tip: The web UI allows you to test your post-upload scripts and provides examples for common use cases.**

### Global Settings

Additional global configuration options:

```yaml
# Global output directory for processed files and NZB files
output_dir: "./output"

# Whether to maintain the original file extension in the NZB filename (default: true)
maintain_original_extension: true
```

> **Note:** For information about file hashing and verification, please see the [File Hash and Verification](file-hash.md) documentation.

## Command Line Parameters

In addition to the configuration file, Postie accepts several command line parameters:

- `-config`: Path to the configuration file
- `-d`: Path to a directory containing files to post
- `-o`: Output directory for processed files
- `-v`: Enable verbose logging
- `-i`: Path to a single file to post. If provided the dir is ignored.
- `-version`: Display version information

Example:

```bash
./postie -config config.yaml -d ./upload -o ./output
```

```bash
./postie watch -config config.yaml -d ./upload -o ./output
```

## Getting Started

**For new users, we strongly recommend starting with the web UI:**

1. Start Postie with minimal or no configuration
2. Open the web interface at `http://localhost:8080`
3. Use the Settings page to configure all options through the intuitive interface
4. The UI will guide you through required settings and validate your configuration
5. Once configured, you can use either the web interface or command line tools

The web UI provides immediate feedback, configuration validation, and organized sections that make setup much easier than manually editing YAML files.
