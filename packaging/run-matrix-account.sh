#!/bin/bash
# Open a persistent personal-account profile, separate from test fixtures.
set -euo pipefail
project_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$project_dir"
if [[ $(uname -s) != Darwin ]]; then
    echo 'This launcher is for macOS.' >&2
    exit 1
fi
export HAGENCY_DESKTOP_DATA_DIR="${HAGENCY_DESKTOP_DATA_DIR:-$HOME/Library/Application Support/Rinx Live}"
mkdir -p "$HAGENCY_DESKTOP_DATA_DIR"
chmod 700 "$HAGENCY_DESKTOP_DATA_DIR"
# Real account credentials must never pass through the test input bridge.
unset MAKEPAD_REMOTE MAKEPAD_HIDE_WINDOWS MAKEPAD_NO_FOCUS MAKEPAD_FOCUS
cargo build --locked --features agent_chat
mkdir -p target/matrix-account
cp target/debug/rinx target/matrix-account/rinx
exec packaging/run-macos.sh "$project_dir/target/matrix-account/rinx"
