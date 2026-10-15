#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"; cd "$ROOT"
PREFIX="${PREFIX:-$HOME/.local}"; STATE="$ROOT/.framework/hoard-deploy-state"; rm -rf "$STATE"; mkdir -p "$STATE/backup"
BINDIR="$PREFIX/bin"; APP="$PREFIX/share/applications/hoard.desktop"; ICON="$PREFIX/share/icons/hicolor/scalable/apps/hoard.svg"; THEME="$PREFIX/share/hoard/themes/hoard.toml"; CONFTHEME="$HOME/.config/hoard/themes/hoard.toml"; SETTINGS="$HOME/.config/hoard/settings.toml"; UPDATE="$HOME/.config/hoard/update.toml"; FWLOCK="$PREFIX/share/hoard/build-info/framework-lock.json"; CARGOLOCK="$PREFIX/share/hoard/build-info/Cargo.lock"; EVIDENCE="$PREFIX/share/hoard/build-info/evidence.json"; INSTALLED_SHA="$PREFIX/share/hoard/build-info/installed.sha256"; BUILD_ID="$PREFIX/share/hoard/build-info/build-id"
for pair in "binary:$BINDIR/hoard" "desktop:$APP" "icon:$ICON" "theme:$THEME" "config-theme:$CONFTHEME" "settings:$SETTINGS" "update:$UPDATE" "framework-lock:$FWLOCK" "cargo-lock:$CARGOLOCK" "evidence:$EVIDENCE" "installed-sha:$INSTALLED_SHA" "build-id:$BUILD_ID"; do key="${pair%%:*}"; path="${pair#*:}"; if [[ -e "$path" ]]; then cp -a "$path" "$STATE/backup/$key"; printf '1\n' > "$STATE/${key}.present"; else printf '0\n' > "$STATE/${key}.present"; fi; done
xdg-mime query default inode/directory > "$STATE/previous-mime" 2>/dev/null || :
printf '%s\n' "$PREFIX" > "$STATE/prefix"
echo 'HOARD PRE-DEPLOY SNAPSHOT PASSED'
