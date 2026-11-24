---
title: Web authentication
sidebar_position: 7
---

# Web authentication

The web UI has no login by default. Turn it on when other people or devices can reach the server.

## Turning it on

Start the web server with `postie-web --auth`, or set `POSTIE_AUTH_ENABLED=true` (for example in Docker
Compose), and restart. `--auth=false` overrides the environment variable. An invalid value stops the server
at startup rather than silently leaving it open.

These are startup options of `postie-web` only. They are not part of `config.yaml`, and the desktop app and
the `postie` command line never ask for a login.

On the first visit, create the administrator account; registration closes once it exists. Usernames need
3 to 128 characters and passwords 12 to 72 bytes. You can sign in with the username or the optional email
address. Create the account before exposing a fresh install to anyone else.

## How it works

- There is one administrator account. There is no password-reset or account-editing screen, so keep the
  credentials safe and back up the database.
- Passwords are hashed with bcrypt. A login sets a signed session cookie (HttpOnly, SameSite=Strict, scoped
  to the host) that lasts 24 hours.
- Each browser login is its own session. Signing out revokes that session and closes its live updates.
  Expired sessions are closed too.
- Accounts and sessions are stored in Postie's SQLite database, so they survive restarts and turning
  authentication off and on again.
- Login and registration are limited to 10 attempts per minute per client IP. Clients behind the same
  reverse proxy share that limit, because forwarded headers are not trusted.

## What needs a login

With authentication on, the web pages, the application API, logs, file browsing and API-key management all
need a session. These keep their own authentication:

- `POST /api/v1/queue/upload` uses the API key ([API and Arr](./integrations.md#http-api)).
- The Sonarr/Radarr webhook uses the `apiKey` query parameter in its URL; other Arr endpoints need a
  session.
- `GET /live` stays public for health checks.

## Hostnames when authentication is off

With authentication off, the server only answers requests addressed to `localhost`, an IP address, or a
hostname in `POSTIE_ALLOWED_HOSTS` (comma separated, with or without the port). Anything else gets `403`,
and the first rejected request per hostname is logged. This stops DNS rebinding, where a malicious website
points its own hostname at your server to reach the unauthenticated UI.

```yaml
environment:
  - POSTIE_ALLOWED_HOSTS=nas.local,postie.example.com
```

With authentication on, any hostname is accepted.

## Behind a reverse proxy

- Forward the original `Host` header (nginx: `proxy_set_header Host $host;`).
- For HTTPS, set `POSTIE_COOKIE_SECURE=true` so the session cookie is marked Secure. Requests that reach
  Postie over TLS directly use Secure cookies automatically.
- Browser requests must come from the same host and scheme the browser sees; cross-origin requests are
  refused.
