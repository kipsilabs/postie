# Postie

[![Rust](https://github.com/kipsilabs/postie/actions/workflows/rust.yml/badge.svg)](https://github.com/kipsilabs/postie/actions/workflows/rust.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

[!["Buy Me A Coffee"](https://www.buymeacoffee.com/assets/img/custom_images/orange_img.png)](https://www.buymeacoffee.com/qbt52hh7sjd)

![logo](./docs/static/img/full_logo.jpeg)

A high-performance Usenet binary poster written in Rust, inspired by Nyuu-Obfuscation.

## Features

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

## Quick Start

### Docker (Recommended)

```bash
# Create docker-compose.yml
curl -o docker-compose.yml https://raw.githubusercontent.com/kipsilabs/postie/main/docker-compose.yml

# Start Postie
docker-compose up -d

# Access web interface at http://localhost:8080
```

#### Authentication and allowed hosts

Set `POSTIE_AUTH_ENABLED=true` to require an administrator login for the web UI.

With authentication off, the web UI only answers requests whose `Host` is `localhost`, an IP address, or
listed in `POSTIE_ALLOWED_HOSTS` (comma separated, with or without port). This protects against
DNS-rebinding attacks, where a malicious website points its own hostname at your server. Anything else
gets `403`, and the first rejected request per hostname is logged to stderr.

```yaml
environment:
  - POSTIE_ALLOWED_HOSTS=nas.local,postie.example.com
```

Behind a reverse proxy, forward the original Host (nginx: `proxy_set_header Host $host;`) and add the
public hostname to `POSTIE_ALLOWED_HOSTS`, or enable authentication instead.

### Homebrew (CLI)

Install the command-line poster on macOS or Linux (Apple Silicon/ARM64 or Intel/AMD64).
Linux requires glibc 2.34+ and the system C++/GCC runtimes:

```bash
brew tap kipsilabs/postie https://github.com/kipsilabs/postie
brew install kipsilabs/postie/postie
postie --help
```

The explicit tap URL uses this repository directly. The formula installs the CLI
as `postie`; configure it with a YAML file ([configuration guide](https://postie.kipsilabs.top/docs/configuration)).
To upgrade after a formula update, run `brew update && brew upgrade kipsilabs/postie/postie`.

### Binary Download

[![Download for Windows](https://img.shields.io/badge/Windows-Download-0078d4?style=for-the-badge&logo=windows)](https://github.com/kipsilabs/postie/releases/latest/download/postie_windows_amd64.zip)
[![Download for macOS](https://img.shields.io/badge/macOS-Download-0078d4?style=for-the-badge&logo=apple)](https://github.com/kipsilabs/postie/releases/latest/download/postie_darwin_amd64.zip)
[![Download for Linux](https://img.shields.io/badge/Linux-Download-0078d4?style=for-the-badge&logo=linux)](https://github.com/kipsilabs/postie/releases/latest/download/postie_linux_amd64.zip)

## Documentation

For detailed documentation, installation instructions, configuration options, and usage examples, please visit the [Postie Documentation Site](https://postie.kipsilabs.top).

## Quick Links

- [Installation Guide](https://postie.kipsilabs.top/docs/installation)
- [Quick Start](https://postie.kipsilabs.top/docs/quick-start)
- [Configuration Guide](https://postie.kipsilabs.top/docs/configuration)
- [Obfuscation Policies](https://postie.kipsilabs.top/docs/obfuscation)
- [File Watcher](https://postie.kipsilabs.top/docs/watcher)

## For Developers

### Prerequisites

- **Rust** (stable, edition 2024) via [rustup](https://rustup.rs)
- **[Bun](https://bun.sh)** for the frontend (do not use npm)
- **CMake** and a C/C++ toolchain, used to build the vendored rapidyenc encoder
  (Xcode command line tools on macOS, MSVC Build Tools on Windows)
- For the desktop app: the [Tauri v2 prerequisites](https://v2.tauri.app/start/prerequisites/)
  and `cargo install tauri-cli --version "^2"`

### Building from Source

`postie-web` serves the compiled frontend from a `frontend/` directory next to its binary
(or `POSTIE_FRONTEND_DIR`), so build the frontend first.

```bash
git clone https://github.com/kipsilabs/postie.git
cd postie

make build            # frontend + postie CLI + postie-web (release)
make build-desktop    # Tauri desktop bundle
```

| Command | Builds | Output |
|---------|--------|--------|
| `make build` | CLI and web server | `rust/target/release/postie`, `rust/target/release/postie-web` |
| `make build-desktop` | Desktop app (Tauri) | `rust/desktop/target/release/bundle/` |
| `make docker-build` | Docker image | `postie:dev` |

Run `make help` for every target, and `make lint test` before opening a pull request.

### Development Mode

```bash
make dev           # postie-web on http://localhost:8080
make desktop-dev   # desktop app
```

## License

This project is licensed under the MIT License - see the LICENSE file for details.

### Duplicate lookup

Optionally reuse an existing NZB before uploading individual files using a generic HTTP lookup service. Disabled by default; reused source files are always retained. See [configuration and provider protocol](docs/duplicate-lookup.md).
