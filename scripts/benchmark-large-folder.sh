#!/usr/bin/env bash
set -euo pipefail
DIR="${1:-$HOME/Pictures}"
[[ -d "$DIR" ]] || { echo "Not a directory: $DIR" >&2; exit 2; }
BIN="${HOARD_BIN:-$HOME/.local/bin/hoard}"
[[ -x "$BIN" ]] || { echo "Installed Hoard binary not found: $BIN (run ./framework/host-gate first)" >&2; exit 1; }
exec "$BIN" --benchmark-dir "$DIR"
