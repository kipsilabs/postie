---
sidebar_position: 8
---

# API and Arr

Two ways for other tools to queue uploads: an HTTP endpoint with an API key, and a Sonarr, Radarr,
Lidarr or Readarr webhook. Both need `postie-web` (Docker, the binaries or the desktop app); the `postie`
command line has neither.

## HTTP API {#http-api}

![External HTTP API](/img/settings/api.png)

Turn on **Settings → Automation → External HTTP API** (`api.enabled: true`). The section shows the API key,
an example request and a **Regenerate** button. Regenerating invalidates the old key.

```bash
curl -X POST http://localhost:8080/api/v1/queue/upload \
  -H "X-API-Key: <your key>" \
  -H "Content-Type: application/json" \
  -d '{"file": "/downloads/tv/Show/episode.mkv", "relative_path": "/downloads/tv", "priority": 0, "delete_after_upload": false}'
```

The key can also be sent as `Authorization: Bearer <key>`.

| Field | Required | Meaning |
|-------|----------|---------|
| `file` | yes | Absolute path of the file, as the Postie server sees it. |
| `relative_path` | yes | Absolute path of a parent folder of `file`. The NZB keeps the file's path below this folder inside `output_dir`. |
| `priority` | no | Queue priority; higher runs first. Default `0`. |
| `delete_after_upload` | no | Delete the file after a successful upload. Default `false`. |

Responses:

| Status | Meaning |
|--------|---------|
| `202` | Queued: `{"status": "queued", "file": "<path>"}` |
| `400` | Missing or invalid field, for example a relative path or a folder that does not contain the file |
| `401` | Missing or wrong API key |
| `403` | The API is turned off |
| `404` | The file does not exist |

The file must be readable by the server. In Docker, use the path inside the container and mount the folder
into it.

## Sonarr and Radarr {#sonarr-and-radarr}

![Arr integration](/img/settings/arr.png)

Under **Settings → Automation → Arr Integration**, click **Add Instance** and fill in:

- **Application type**: Radarr, Sonarr, Lidarr or Readarr.
- **URL** and **API key** of that app (its API key is under *Settings → General* in the app).
- **Delete file from library after upload**, if Postie should remove the imported file once it is posted.

**Test** checks the connection. **Setup webhook** registers a webhook named `Postie` in the app, triggered on
import and upgrade. From then on, every file the app imports is queued in Postie. Removing the instance
deletes the webhook again.

Two things to get right:

- **Open Postie at an address the Arr app can reach before you click Setup webhook.** The webhook points at
  the address in your browser's address bar. If you opened Postie as `http://localhost:8080` and the Arr app
  runs in another container or machine, it cannot reach that; open Postie by its LAN IP or hostname first.
- **Postie must see the imported files at the same paths as the Arr app.** The webhook sends the app's own
  file paths. In Docker, mount the media folders at the same paths in both containers.

The webhook authenticates with the `apiKey` in its URL, so it keeps working when
[web authentication](./web-authentication.md) is on.
