#!/bin/sh
# Launches the Rust web server with a fresh throwaway HOME on every start (like rust/original/run.sh), so
# replay never touches real data. Build first: (cd rust/server && cargo build --release)
D="$(cd "$(dirname "$0")" && pwd)"
H="$(mktemp -d "${TMPDIR:-/tmp}/postie-rust-XXXXXX")"
export HOME="$H" XDG_CONFIG_HOME="$H/.config" XDG_DATA_HOME="$H/.local/share"
export POSTIE_FRONTEND_DIR="${POSTIE_FRONTEND_DIR:-$D/../../frontend/build}"
# the version a release build bakes in; the recorded original reported its Go build version
export POSTIE_VERSION="${POSTIE_VERSION:-v0.0.36-0.20261118212959-aa2a698522a5+dirty}"
exec "$D/target/release/postie-web"
