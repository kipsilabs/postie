#!/bin/sh
# Launches the Go original (cmd/web) with a throwaway HOME so record/replay never touch real data.
# Build first: go build -o rust/original/postie-web ./cmd/web   (needs a stub frontend/build/index.html)
D="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$D/home"
export HOME="$D/home" XDG_CONFIG_HOME="$D/home/.config" XDG_DATA_HOME="$D/home/.local/share"
exec "$D/postie-web"
