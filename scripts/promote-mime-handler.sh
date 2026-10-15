#!/usr/bin/env bash
set -euo pipefail
PREFIX="${PREFIX:-$HOME/.local}"
APPDIR="$PREFIX/share/applications"
command -v xdg-mime >/dev/null 2>&1 || { echo 'xdg-mime required for directory handler promotion' >&2; exit 1; }
if command -v update-desktop-database >/dev/null 2>&1; then update-desktop-database "$APPDIR" >/dev/null 2>&1 || true; fi
xdg-mime default hoard.desktop inode/directory
HANDLER="$(xdg-mime query default inode/directory 2>/dev/null || true)"
[[ "$HANDLER" == 'hoard.desktop' ]] || { echo "directory-handler verification failed: ${HANDLER:-<empty>}" >&2; exit 1; }
if command -v gtk-update-icon-cache >/dev/null 2>&1; then gtk-update-icon-cache -f -t "$PREFIX/share/icons/hicolor" >/dev/null 2>&1 || true; fi
echo 'HOARD MIME PROMOTION PASSED: inode/directory -> hoard.desktop'
