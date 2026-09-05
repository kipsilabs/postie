---
sidebar_position: 2
---

# Installation

There are several ways to install and run Postie. Choose the method that best suits your environment and needs.

**💡 All installation methods include access to the modern web interface for easy configuration and monitoring.**

## Quick Download

Download the latest release for your platform:

<a href="https://github.com/kipsilabs/postie/releases/latest/download/postie-gui-windows-amd64.zip">
  <img src="/img/download-for-windows.webp" alt="Download for Windows" width="200" height="60" />
</a>
<a href="https://github.com/kipsilabs/postie/releases/latest/download/postie-gui-macos-universal.zip">
  <img src="/img/download-for-mac.png" alt="Download for macOS" width="200" height="60" />
</a>

[![View All Releases](https://img.shields.io/badge/All%20Releases-View-6c757d?style=for-the-badge&logo=github)](https://github.com/kipsilabs/postie/releases/latest)

## Binary Installation

The easiest way to get started is by downloading a pre-built binary:

1. Go to the [releases page](https://github.com/kipsilabs/postie/releases) or use the download buttons above
2. Download the appropriate binary for your platform
3. Extract the archive
4. Run Postie to start the web interface:

```bash
./postie
```

5. Open your browser to `http://localhost:8080` and configure through the web UI

### Alternative: Command Line Configuration

If you prefer to use a configuration file:

1. Create a config.yaml file ([see configuration guide](./configuration.md))
2. Run Postie with the configuration file:

```bash
./postie -config config.yaml -d ./upload -o ./output
```

## Docker Installation

### Multi-Architecture Support

Postie Docker images support multiple architectures:
- **AMD64 (x86_64)**: Intel/AMD 64-bit processors
- **ARM64 (aarch64)**: ARM 64-bit processors (Raspberry Pi 4/5, Apple Silicon, AWS Graviton, etc.)

Docker will automatically pull the correct image for your platform using manifest lists.

### Using Docker Compose (Recommended)

Postie provides a complete Docker setup that includes the web interface for easy management.

1. Create a `docker-compose.yml` file:

```yaml
services:
  postie:
    container_name: postie
    image: ghcr.io/kipsilabs/postie:latest
    ports:
      - "8080:8080"
    volumes:
      - ./config:/config
      - ./watch:/watch
      - ./output:/output
    environment:
      - PORT=8080
      - HOST=0.0.0.0
      - PUID=1000
      - PGID=1000
    restart: unless-stopped
```

#### Platform-Specific Configuration

For ARM devices like Raspberry Pi, you may want to explicitly specify the platform:

```yaml
services:
  postie:
    container_name: postie
    image: ghcr.io/kipsilabs/postie:latest
    platform: linux/arm64  # Explicitly specify ARM64
    ports:
      - "8080:8080"
    volumes:
      - ./config:/config
      - ./watch:/watch
      - ./output:/output
    environment:
      - PORT=8080
      - HOST=0.0.0.0
      - PUID=1000
      - PGID=1000
    restart: unless-stopped
```

1. Create the following directories:

   - `config`: Contains your configuration file
   - `watch`: Directory to watch for new files
   - `output`: Directory for output files

1. Create a `config.yaml` file in the `config` directory ([see configuration guide](./configuration.md))

1. Start the container:

```bash
docker-compose up -d
```

1. Access the web interface at `http://localhost:8080`

### Using Docker Run

Alternatively, you can run Postie directly with Docker:

```bash
docker run -d \
  --name postie \
  -p 8080:8080 \
  -v ./config:/config \
  -v ./watch:/watch \
  -v ./output:/output \
  -e PUID=1000 \
  -e PGID=1000 \
  ghcr.io/kipsilabs/postie:latest
```

#### ARM Device Configuration

For ARM-based devices (Raspberry Pi, ARM servers), you can explicitly specify the platform:

```bash
docker run -d \
  --name postie \
  --platform linux/arm64 \
  -p 8080:8080 \
  -v ./config:/config \
  -v ./watch:/watch \
  -v ./output:/output \
  -e PUID=1000 \
  -e PGID=1000 \
  ghcr.io/kipsilabs/postie:latest
```

#### Available Image Tags

You can also use architecture-specific tags if needed:

- `ghcr.io/kipsilabs/postie:latest` - Multi-arch manifest (recommended)
- `ghcr.io/kipsilabs/postie:v{version}` - Multi-arch manifest for specific version
- `ghcr.io/kipsilabs/postie:v{version}-amd64` - AMD64 specific
- `ghcr.io/kipsilabs/postie:v{version}-arm64` - ARM64 specific

### Web Interface Access

After starting the Docker container, you can access the Postie web interface by:

1. Opening your web browser
2. Navigating to `http://localhost:8080`
3. Using the web interface to:
   - Monitor upload progress
   - Manage the upload queue
   - Configure settings
   - View logs and statistics

The web interface provides a modern, user-friendly way to interact with Postie without needing to use command-line tools.

### ARM Device Specific Notes

#### Performance Considerations
- **Raspberry Pi 4/5**: Works well with default settings. Consider adjusting concurrent uploads based on available RAM
- **ARM servers**: Generally provide excellent performance similar to AMD64
- **Older ARM devices**: May need reduced concurrency settings in configuration

#### Common ARM Issues and Solutions

**Issue: "no matching manifest" error**
```bash
# Solution: Explicitly specify the platform
docker run --platform linux/arm64 ...
```

**Issue: Poor performance on Raspberry Pi**
```yaml
# In your config.yaml, reduce concurrent operations:
queue:
  max_concurrent_uploads: 2  # Reduced from default
```

**Issue: Memory constraints**
```yaml
# Adjust chunk sizes for memory-limited devices:
posting:
  article_size_in_bytes: 500000  # Smaller chunks for less memory usage
```

#### Raspberry Pi Optimization

For optimal performance on Raspberry Pi:

1. **Use fast storage**: SD cards can be slow; consider USB 3.0 drives
2. **Monitor temperature**: Use `docker stats` to monitor resource usage
3. **Adjust memory settings**:

```yaml
services:
  postie:
    # ... other configuration
    deploy:
      resources:
        limits:
          memory: 512M  # Adjust based on available RAM
    environment:
      - GOMAXPROCS=2  # Limit to available CPU cores
```

## Using the Binary Version

After downloading and extracting the binary for your platform:

### Windows

1. Extract the downloaded zip file
2. Create a `config.yaml` file ([see configuration guide](./configuration.md))
3. Open Command Prompt or PowerShell
4. Navigate to the extracted folder
5. Run Postie:

```cmd
postie.exe -config config.yaml -d ./upload -o ./output
```

### macOS

1. Extract the downloaded tar.gz file
2. Create a `config.yaml` file ([see configuration guide](./configuration.md))
3. Open Terminal
4. Navigate to the extracted folder
5. Make the binary executable:

```bash
chmod +x postie
```

1. Run Postie:

```bash
./postie -config config.yaml -d ./upload -o ./output
```

### Linux

1. Extract the downloaded tar.gz file
2. Create a `config.yaml` file ([see configuration guide](./configuration.md))
3. Open Terminal
4. Navigate to the extracted folder
5. Make the binary executable:

```bash
chmod +x postie
```

1. Run Postie:

```bash
./postie -config config.yaml -d ./upload -o ./output
```

#### ARM Linux (Raspberry Pi, ARM servers)

The project provides native ARM64 binaries for Linux:

1. Download the ARM64 binary from the [releases page](https://github.com/kipsilabs/postie/releases)
   - Look for files ending in `-linux-arm64.tar.gz`
2. Follow the same Linux installation steps above
3. The ARM64 binary provides optimal performance on ARM devices

**Performance tip for ARM devices**: Start with reduced concurrent uploads:
```bash
./postie -config config.yaml -d ./upload -o ./output --max-concurrent=2
```

#### Web Interface with Binary

1. Download [postie-web](https://github.com/kipsilabs/postie/releases/latest/download/postie-web-linux-amd64.tar.gz)
2. Extract the file
3. Start Postie with the web server enabled `./postie-web --port 80080`
4. Open your web browser
5. Navigate to `http://localhost:8080` (or the port specified in your configuration)
6. Use the same web interface features as the Docker version

## Building from Source

If you prefer to build from source:

```bash
git clone https://github.com/kipsilabs/postie.git
cd postie
go build
```

## Installing with Go

If you have Go installed, you can install Postie directly:

```bash
go install github.com/kipsilabs/postie@latest
```

## System Requirements

- **OS**: Linux, macOS, or Windows
- **Architecture**: AMD64 (x86_64) or ARM64 (aarch64)
  - **AMD64**: Intel/AMD 64-bit processors
  - **ARM64**: ARM 64-bit processors (Raspberry Pi 4+, Apple Silicon, AWS Graviton, etc.)
- **Memory**: 
  - Minimum: 256MB RAM
  - Recommended: 512MB+ RAM for optimal performance
  - ARM devices: 512MB+ recommended for concurrent uploads
- **Disk Space**: Varies based on file sizes being processed
- **Network**: Reliable internet connection with sufficient bandwidth for Usenet posting

### Hardware-Specific Notes

- **Raspberry Pi 4/5**: Excellent performance with 4GB+ RAM models
- **Raspberry Pi 3**: Supported but may need reduced concurrency settings
- **Apple Silicon Macs**: Native ARM64 support via Docker or binary
- **ARM servers** (AWS Graviton, Oracle Ampere): Full performance equivalent to AMD64

## Next Steps

After installation, you'll need to [configure Postie](./configuration) before you can start posting files.
