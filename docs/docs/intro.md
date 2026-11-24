---
sidebar_position: 1
---

# Introduction to Postie

Postie is a high-performance Usenet binary poster written in Go, inspired by Nyuu-Obfuscation.

## What is Postie?

Postie is designed to efficiently upload binary files to Usenet with a focus on:

- **Performance**: Optimized for high-speed posting
- **Reliability**: Automatic retry and post verification
- **Flexibility**: Multiple configuration options for different posting strategies
- **Security**: Various obfuscation policies to protect your posts

## Key Features

- **Modern Web Interface**: Intuitive web UI for configuration, uploading, and monitoring
- Multi-server support with automatic failover
- Yenc encoding using rapidyenc for high performance
- Post checking with multiple attempts
- Configurable segment size
- Automatic retry on failure
- SSL/TLS support
- Connection pooling for better performance
- PAR2 support with configurable redundancy
- Multiple obfuscation policies
- Configurable group posting policies
- File watching and automatic posting
- Configurable posting schedules
- Robust file hashing for integrity verification
- Upload queue management with pause/resume functionality
- Real-time upload progress monitoring

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
3. Use the Settings page to configure your servers and preferences
4. Start uploading files through the intuitive web interface

For command-line usage and detailed configuration options, see the [Quick Start](./quick-start) and [Configuration Guide](./configuration) sections.

For information about file hashing and verification, see the [File Hash and Verification](./file-hash) guide.
