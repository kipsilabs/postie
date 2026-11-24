---
sidebar_position: 2
---

# Installation

Postie comes in four forms. Pick the one that fits how you want to run it:

| Form | What you get | Best for |
|------|--------------|----------|
| **Desktop app** | A native window around the web UI | Running Postie on your own computer |
| **Docker** | `postie-web` in a container, web UI on port 8080 | Servers, NAS boxes, always-on setups |
| **Binaries** | `postie-web` (web UI) and `postie` (command line) | Servers without Docker, scripting |
| **Homebrew** | The `postie` command-line poster | macOS and Linux terminals |

## Desktop app

<a href="https://github.com/kipsilabs/postie/releases/latest/download/Postie-windows-x64-setup.exe">
  <img src="/img/download-for-windows.webp" alt="Download for Windows" width="200" height="60" />
</a>
<a href="https://github.com/kipsilabs/postie/releases/latest/download/Postie-macos-arm64.dmg">
  <img src="/img/download-for-mac.png" alt="Download for macOS" width="200" height="60" />
</a>

[![All downloads](https://img.shields.io/badge/All%20Downloads-View-6c757d?style=for-the-badge&logo=github)](https://github.com/kipsilabs/postie/releases/latest)

| Platform | Download |
|----------|----------|
| Windows x64 | `Postie_<version>_x64-setup.exe` (installer) or `Postie_<version>_x64_en-US.msi` |
| macOS (Apple Silicon) | `Postie_<version>_aarch64.dmg` |
| Linux x86_64 | `Postie_<version>_amd64.AppImage`, `.deb` or `.rpm` |

Intel Macs are not supported by the desktop app. Use [Docker](#docker) instead.

The bundles are not signed with a paid developer certificate:

- **macOS** says it cannot verify the app on first launch. Open **System Settings → Privacy & Security** and
  click **Open Anyway**, or run `xattr -dr com.apple.quarantine /Applications/Postie.app`.
- **Windows SmartScreen** may warn about an unknown publisher. Click **More info → Run anyway**.

The [setup wizard](./first-setup.md) opens on first launch.

## Docker

Images are published to `ghcr.io/kipsilabs/postie` for **amd64** and **arm64** (Raspberry Pi 4/5, Apple
Silicon, AWS Graviton). Docker pulls the right one for your machine.

| Tag | Meaning |
|-----|---------|
| `latest` | The newest stable release |
| `vX.Y.Z` | A specific release, for example `v0.1.0` |
| `vX.Y.Z-rc.N` | A release candidate (never tagged `latest`) |

### Docker Compose (recommended)

```yaml
services:
  postie:
    container_name: postie
    image: ghcr.io/kipsilabs/postie:latest
    ports:
      - "8080:8080"
    volumes:
      - ./config:/config # config.yaml, the database and logs
      - ./watch:/watch # point a watcher here
      - ./output:/output # set the output directory to /output
    environment:
      - PUID=1000
      - PGID=1000
      # With authentication off, the UI only answers localhost, IP addresses and these hostnames:
      # - POSTIE_ALLOWED_HOSTS=nas.local,postie.example.com
      # Require an administrator login instead:
      # - POSTIE_AUTH_ENABLED=true
    restart: unless-stopped
```

```bash
docker compose up -d
```

Open `http://localhost:8080` (or `http://<server-ip>:8080`). The setup wizard runs the first time. In the
wizard, set the output directory to `/output` so NZBs land in the mounted folder, and use `/watch` as the
watch directory if you set up a watcher.

### Docker run

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

### Opening the UI by hostname

With authentication off, Postie only answers requests addressed to `localhost`, an IP address, or a
hostname listed in `POSTIE_ALLOWED_HOSTS`. This blocks DNS-rebinding attacks, where a malicious web page
points its own hostname at your server. If you open the UI as `http://nas.local:8080` or through a reverse
proxy, either add that hostname to `POSTIE_ALLOWED_HOSTS` (comma separated, with or without the port) or
[enable authentication](./web-authentication.md). Rejected hostnames get `403` and are logged once.

### Container settings

| Variable | Default | Meaning |
|----------|---------|---------|
| `PUID` / `PGID` | `1000` | User and group the server runs as (owner of the mounted folders) |
| `PORT` | `8080` | Port inside the container |
| `HOST` | `0.0.0.0` | Address inside the container |
| `POSTIE_AUTH_ENABLED` | `false` | Require an administrator login ([details](./web-authentication.md)) |
| `POSTIE_ALLOWED_HOSTS` | | Extra hostnames accepted while authentication is off |
| `POSTIE_COOKIE_SECURE` | `false` | Mark the session cookie Secure, behind an HTTPS reverse proxy |

The health check endpoint is `GET /live`.

## Binaries

Each release has an archive per platform:

| Platform | Archive |
|----------|---------|
| Linux x86_64 | `postie-rust-<tag>-linux-x86_64.tar.gz` |
| Linux arm64 | `postie-rust-<tag>-linux-arm64.tar.gz` |
| macOS (Apple Silicon) | `postie-rust-<tag>-macos-arm64.tar.gz` |
| Windows x64 | `postie-rust-<tag>-windows-x86_64.zip` |

Download it from the [releases page](https://github.com/kipsilabs/postie/releases/latest) and extract it.
The Linux binaries need glibc 2.35 or newer (Ubuntu 22.04, Debian 12 or later).
It contains:

- `postie-web`: the web server and UI.
- `frontend/`: the UI files. Keep this folder next to `postie-web`, or point `POSTIE_FRONTEND_DIR` at it.
- `postie`: the command-line poster.

### Web UI

```bash
./postie-web            # http://localhost:8080
./postie-web --port 9090 --host 127.0.0.1
```

Run `./postie-web --help` for every flag. `postie-web` keeps its config, database and log in a fixed
per-user directory (the same one the desktop app uses):

| OS | Config and database | Log |
|----|---------------------|-----|
| Linux | `~/.config/postie/` (or `$XDG_CONFIG_HOME/postie/`) | `~/.local/share/postie/postie.log` |
| macOS | `~/Library/Application Support/Postie/` | same folder |
| Windows | `%APPDATA%\Postie\` | same folder |
| Docker | `/config/` | `/config/postie.log` |

### Command line

`postie` uploads files once and exits, or watches a directory with `postie watch`. It reads a YAML config
file (default `./config.yaml`) and does not start the web UI.

```bash
./postie -c config.yaml -i /path/to/file.mkv -o ./nzb   # one file
./postie -c config.yaml -d /path/to/folder -o ./nzb     # every file in a folder
./postie watch -c config.yaml -d /path/to/watch -o ./nzb  # keep watching a folder
```

:::warning
Without `-i`, `postie` uploads every file in `--dir`, which defaults to the current directory. Always pass
`-i` or `-d`.
:::

See [command line usage](./configuration.md#command-line) for every flag.

## Homebrew (CLI)

On Apple Silicon Macs and on Linux (x86_64 or arm64):

```bash
brew tap kipsilabs/postie https://github.com/kipsilabs/postie
brew install kipsilabs/postie/postie
postie --help
```

The explicit repository URL is needed because the formula lives in the Postie repository. Homebrew
installs only the `postie` command line; for the web UI use Docker, the desktop app or the binaries.
Upgrade with `brew update && brew upgrade kipsilabs/postie/postie`.

## Building from source

You need Rust (stable), [Bun](https://bun.sh), CMake and a C/C++ toolchain:

```bash
git clone https://github.com/kipsilabs/postie.git
cd postie
make build          # frontend + rust/target/release/postie and postie-web
make build-desktop  # desktop bundle in rust/desktop/target/release/bundle/
```

## System requirements

- **OS:** Linux (x86_64, arm64), macOS on Apple Silicon, or Windows x64. Docker covers amd64 and arm64.
- **Memory:** the web server idles at a few tens of MB. The upload buffer is sized automatically between
  64 MiB and 512 MiB (`posting.upload_buffer_memory_limit`), and PAR2 creation can use up to
  `par2.memory_limit` (4 GiB by default). On small devices, lower both.
- **Disk:** PAR2 files are written to `par2.temp_dir` (your system temp folder by default) while a job
  runs, roughly the size of the source times the redundancy.

## Next steps

- [First setup](./first-setup.md): the setup wizard, step by step.
- [Configuration](./configuration.md): every setting, in the UI and in YAML.
