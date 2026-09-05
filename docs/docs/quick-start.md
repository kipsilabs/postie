---
sidebar_position: 3
---

# Quick Start

This guide will help you get up and running with Postie quickly.

## Prerequisites

Before you begin, make sure you have:

- Installed Postie using one of the [installation methods](./installation)
- Access to at least one Usenet server with posting permissions
- Files you want to post to Usenet

## Getting Started with the Web Interface (Recommended)

**The easiest way to get started with Postie is through the web interface.** The web UI provides a user-friendly setup process with real-time validation and guidance.

### Step 1: Start Postie

Start Postie using Docker or the binary:

**Docker:**

```bash
docker run -p 8080:8080 -v $(pwd)/config:/config -v $(pwd)/output:/output kipsilabs/postie
```

**Binary:**

```bash
./postie
```

### Step 2: Open the Web Interface

1. Open your web browser and navigate to `http://localhost:8080`
2. You'll be prompted to configure Postie if this is your first time

### Step 3: Configure Through the Web UI

1. **Navigate to Settings**: Click on the Settings tab
2. **Configure Servers**: Add your Usenet server details with automatic validation
3. **Set Upload Preferences**: Configure posting options with helpful tooltips
4. **Review and Save**: The UI will validate your configuration and show any issues

The web interface organizes settings into logical tabs:

- **Core Configuration**: Servers and basic settings
- **Upload Settings**: Posting configuration and connection management
- **File Processing**: PAR2 and NZB compression settings
- **Automation**: File watcher and post-upload scripts

### Step 4: Start Uploading

Once configured, you can:

- **Drag and drop files** directly into the web interface
- **Monitor uploads** in real-time with detailed progress
- **Manage the upload queue** with pause, resume, and retry options

## Alternative: Manual Configuration

If you prefer to configure Postie manually with a YAML file, create a `config.yaml` file:

```yaml
servers:
  - host: news.example.com
    port: 119 # or 563 for SSL
    username: your_username
    password: your_password
    ssl: true
    max_connections: 10

posting:
  max_retries: 3
  retry_delay: 5s
  article_size_in_bytes: 750000 # ~750KB per article
  groups:
    - name: alt.binaries.test
      enabled: true
  obfuscation_policy: full # Options: full, partial, none

post_check:
  enabled: true
  delay: 10s
  max_reposts: 1

output_dir: "./output"
```

Save this file and start Postie with: `./postie -config config.yaml`

**💡 Note**: Even with manual configuration, we recommend using the web interface for uploading and monitoring, as it provides a much better user experience.

## Command Line Usage (Alternative)

For advanced users who prefer command-line operations:

### Posting a Single File

To post a single file:

```bash
./postie -config config.yaml -i /path/to/your/file.mp4 -o ./output
```

### Posting a Directory

To post all files in a directory:

```bash
./postie -config config.yaml -d /path/to/directory -o ./output
```

### Watch Mode

To continuously watch a directory for new files to post:

```bash
./postie watch -config config.yaml -d /path/to/watch_dir -o ./output
```

## Verifying Posts

If you've enabled post checking in your configuration (`post_check.enabled: true`), Postie will automatically verify that your posts were successful after a specified delay. This is enabled by default and provides peace of mind that your uploads completed successfully.

## Next Steps

Now that you have Postie running:

- Explore the [detailed configuration options](./configuration) via the web UI or YAML
- Learn about [obfuscation policies](./obfuscation) to protect your uploads
- Set up [automated posting with the file watcher](./watcher) for hands-off uploading
- Configure [queue management](./configuration#queue-management) for handling large upload batches

**💡 Pro Tip**: The web interface provides tooltips and help text for all configuration options, making it easy to understand what each setting does without referring to documentation.
