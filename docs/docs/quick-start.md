---
sidebar_position: 3
---

# Quick Start

You need a Usenet account that allows posting, and Postie [installed](./installation).

## With the web interface

1. **Start Postie.**
   - Desktop app: open it.
   - Docker:

     ```bash
     docker run -d -p 8080:8080 -v ./config:/config -v ./output:/output ghcr.io/kipsilabs/postie:latest
     ```

   - Binaries: run `./postie-web` from the extracted folder.
2. **Open the UI.** The desktop app opens it for you; otherwise go to `http://localhost:8080`.
3. **Run the setup wizard.** It appears the first time and asks for your server and an output folder. See
   [First Setup](./first-setup) for each step.
4. **Upload.** Drag files or folders onto the dashboard, or use its add buttons. Progress, verification and
   the queue update live; finished NZBs can be downloaded from the queue.

Everything else lives under **Settings**, in four tabs: Core Configuration, Upload Settings, File
Processing and Automation. See [Configuration](./configuration) for every option.

## With the command line

Create a `config.yaml`:

```yaml
servers:
  - host: news.example.com
    port: 563
    username: your_username
    password: your_password
    ssl: true
    max_connections: 10

posting:
  groups:
    - name: alt.binaries.test
      enabled: true
```

Everything you leave out uses its default: full obfuscation, 5% PAR2 and post verification on.

```bash
# one file
postie -c config.yaml -i /path/to/file.mkv -o ./nzb

# every file in a folder, skipping partial downloads
postie -c config.yaml -d /path/to/folder -o ./nzb --ignore-pattern '*.part' --ignore-pattern '*.tmp'

# keep watching a folder
postie watch -c config.yaml -d /path/to/watch -o ./nzb
```

Without `-i`, `postie` uploads everything in `-d`, which defaults to the current directory. Run
`postie --help` for all flags.

## Verification

Post checking is on by default: after posting, Postie asks the servers whether every article arrived and
reposts any that are missing. With a [verify server](./configuration#verify-servers) on another provider,
it also confirms the articles reached that backbone.

## Next steps

- [Obfuscation](./obfuscation) to choose how much of each post is hidden.
- [File Watcher](./watcher) for hands-off uploads.
- [API and Arr](./integrations) to connect other tools.
