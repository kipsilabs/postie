---
sidebar_position: 1
---

# Introduction to Postie

Postie is a high-performance Usenet binary poster written in Rust, inspired by Nyuu-Obfuscation. It runs as a desktop app, a Docker container, a web server or a command-line tool.

## What is Postie?

Postie is designed to efficiently upload binary files to Usenet with a focus on:

- **Performance**: Optimized for high-speed posting
- **Reliability**: Automatic retry and post verification
- **Flexibility**: Multiple configuration options for different posting strategies
- **Security**: Various obfuscation policies to protect your posts

## Key Features

- **Web interface** for setup, uploads, the queue, metrics and logs, also packaged as a desktop app
- Several servers at once, with separate verify-only servers on other backbones
- yEnc encoding with rapidyenc (SIMD) and pipelined NNTP posting over TLS
- Built-in PAR2 creation, overlapped with the upload
- Post verification (`STAT`) during and after the upload, with automatic reposts
- Three obfuscation policies, for files and PAR2 separately
- Multiple file watchers with schedules, filters and optional source deletion
- Persistent upload queue with priorities, pause and retry
- HTTP API, Sonarr/Radarr/Lidarr/Readarr webhooks and post-upload scripts
- Optional [duplicate lookup](./duplicate-lookup.md) to reuse existing NZBs

## Screenshots

Postie comes with a modern web interface that makes it easy to manage your uploads and monitor progress. **We recommend using the web UI for the best user experience.**

### Dashboard

![Dashboard](/img/examples/dashboard.png)
*Queue statistics, live upload progress and background verification, updated in real time*

### Upload Queue

![Queue](/img/examples/queue.png)
*The upload queue on the dashboard: filter by status, search, reprioritize, retry failed items and download finished NZBs*

### Settings

![Settings](/img/examples/settings.png)
*Settings grouped into Core Configuration, Upload Settings, File Processing and Automation tabs; changes save automatically*

### Metrics

![Metrics](/img/examples/metrics.png)
*Live transfer metrics: upload workers, buffer memory, PAR2 jobs, speed and per-server connection stats*

### Logs

![Logs](/img/examples/logs.png)
*Live log viewer with filtering by text and level, plus copy and download*

## Why Use Postie?

Postie provides a robust solution for uploading content to Usenet with features that ensure your posts are:

1. **Fast**: Optimized for high-throughput posting with efficient connection management
2. **Reliable**: Post verification and automatic reposting of failed segments
3. **Secure**: Multiple obfuscation options to protect your content
4. **Flexible**: Extensive configuration options to meet your specific needs
5. **User-Friendly**: Modern web interface with real-time monitoring and intuitive controls

## Getting Started

**We recommend starting with the web interface for the easiest setup experience:**

1. Follow the [Installation Guide](./installation) to get Postie running
2. Open the web interface at `http://localhost:8080`
3. Follow the [setup wizard](./first-setup) to add your servers
4. Start uploading files through the intuitive web interface

For command-line usage and detailed configuration options, see the [Quick Start](./quick-start) and [Configuration Guide](./configuration) sections.
