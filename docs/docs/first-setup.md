---
sidebar_position: 3
---

# First Setup

This tutorial walks you through Postie's 3-step setup wizard from a fresh install to a working configuration.

## Installation

Before starting, install Postie using one of the available methods — see the [Installation guide](./installation) for full details.

| Method | How to run |
|--------|-----------|
| **Desktop App** (Windows / macOS) | Download from [GitHub Releases](https://github.com/kipsilabs/postie/releases/latest) and launch the app |
| **Docker** | `docker run -p 8080:8080 -v $(pwd)/config:/config kipsilabs/postie` |
| **Binary** | Download from Releases, then run `./postie` |

> 💡 The setup wizard launches automatically the first time you open Postie.

## Opening the Setup Wizard

- **Desktop App** — the wizard opens automatically on first launch.
- **Docker / Binary** — open `http://localhost:8080` in your browser; the wizard appears when no configuration exists.

---

## Step 1 — Welcome

![Step 1 - Welcome screen](/img/setup/step1-welcome.png)

On the first screen you can choose:

- **Language** — English, Spanish, French, Turkish
- **Theme** — System Default, Light, Dark, or one of the many colour themes (Cupcake, Dracula, Nord, etc.)

Click **Next** when ready.

---

## Step 2 — NNTP Servers

![Step 2 - Empty server form](/img/setup/step2-servers-empty.png)

Configure the NNTP servers Postie will use for uploading.

### Upload Pool

The Upload Pool is required. Add your Usenet provider's credentials here. You can add multiple providers to maximise upload bandwidth.

**Fields:**

| Field | Description | Example |
|-------|-------------|---------|
| **Host** | Your provider's hostname | `news.example.com` |
| **Port** | Connection port | `119` (plain) or `563` (SSL) |
| **Use SSL/TLS** | Enable encrypted connection | leave unchecked for local |
| **Username** | Your account username | leave blank for local |
| **Password** | Your account password | leave blank for local |
| **Max Connections** | Parallel connections to open | `10` |

### Testing the connection

After entering credentials, click **Test Connection**. A green **✓ Valid** badge appears if the server is reachable.

![Step 2 - Valid connection](/img/setup/step2-servers-valid.png)

> 💡 For real Usenet providers, use port `119` for plain-text or `563` for SSL/TLS connections.

### Verify Pool *(optional)*

The Verify Pool uses servers from other providers to confirm that articles have propagated across the Usenet backbone. Leave it empty to use the Upload Pool for verification.

Click **Next** once the Upload Pool shows a valid connection.

---

## Step 3 — Output Directory

![Step 3 - Directories](/img/setup/step3-directories.png)

Set where Postie saves the generated NZB files.

| Field | Description | Default |
|-------|-------------|---------|
| **Output Path** | Directory for NZB output files | `./output` |

The default `./output` creates a folder next to the Postie binary. Click **Finish Setup** to save the configuration and open the dashboard.

---

## You're Ready!

Postie is now configured. You'll be redirected to the dashboard where you can start uploading files.

## Next Steps

- [Configuration](./configuration) — full reference for all settings
- [File Watcher](./watcher) — automate uploads by watching a directory
- [Obfuscation](./obfuscation) — privacy options for your uploads
