#!/usr/bin/env bash
set -euo pipefail
PREFIX="${PREFIX:-$HOME/.local}"
systemctl --user disable --now hoard-update.timer 2>/dev/null || true
rm -f "$HOME/.config/systemd/user/hoard-update.service" "$HOME/.config/systemd/user/hoard-update.timer"
systemctl --user daemon-reload 2>/dev/null || true
rm -f "$PREFIX/bin/hoard" "$PREFIX/share/applications/hoard.desktop" "$PREFIX/share/icons/hicolor/scalable/apps/hoard.svg"
rm -rf "$PREFIX/share/hoard"
echo "Hoard removed. User config retained at ~/.config/hoard"
