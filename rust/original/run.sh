#!/bin/sh
# Launches the Go original (cmd/web) with a fresh throwaway HOME on every start, so record/replay
# never touch real data and logs do not accumulate across runs. The directory lives under the OS
# temp dir (cleaned by the OS); nothing here deletes files.
# Build first: go build -o rust/original/postie-web ./cmd/web   (needs a stub frontend/build/index.html)
D="$(cd "$(dirname "$0")" && pwd)"
H="$(mktemp -d "${TMPDIR:-/tmp}/postie-original-XXXXXX")"
export HOME="$H" XDG_CONFIG_HOME="$H/.config" XDG_DATA_HOME="$H/.local/share"
exec "$D/postie-web"
