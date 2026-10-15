#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
fail() { printf 'HOARD API3 PARITY PROOF FAILED: %s\n' "$*" >&2; exit 1; }
require_file() { [[ -f "$1" ]] || fail "missing required file: $1"; }
require_source() {
  local pattern="$1" file="$2" message="$3"
  grep -Eq "$pattern" "$file" || fail "$message"
}

APP='crates/hoard-function/src/app.rs'
PLATFORM='apps/hoard/src/platform.rs'
DEPLOY='scripts/deploy-user.sh'
INSTALLED='scripts/installed-runtime-hook.sh'
ROLLBACK='scripts/rollback-user.sh'
PROMOTE='scripts/promote-mime-handler.sh'

for path in "$APP" "$PLATFORM" "$DEPLOY" "$INSTALLED" "$ROLLBACK" "$PROMOTE"; do
  require_file "$path"
done

# Core file-management parity exists in first-party implementation and is exercised by --self-test.
require_source 'fn new_tab\(' "$APP" 'tab creation parity missing'
require_source 'fn close_tab\(' "$APP" 'tab close parity missing'
require_source 'ClipboardMode::Copy' "$APP" 'copy parity missing'
require_source 'ClipboardMode::Move' "$APP" 'cut parity missing'
require_source 'fn paste_' "$APP" 'paste parity missing'
require_source 'QueuedOperation::Rename' "$APP" 'rename parity missing'
require_source 'QueuedOperation::Trash' "$APP" 'trash UI parity missing'
require_source 'restore_selected_from_trash' "$APP" 'multi-selection restore UI parity missing'
require_source 'Open With' "$APP" 'open-with UI parity missing'
require_source 'save_pinned' "$APP" 'pinned-location persistence parity missing'
require_source 'UndoHistory' "$APP" 'undo/redo integration missing'

# Native Wayland/Vulkan + input contracts.
require_source 'Backends::VULKAN' "$PLATFORM" 'Vulkan-only backend contract missing'
require_source 'create_drag_and_drop_source' "$PLATFORM" 'outgoing Wayland DnD missing'
require_source 'finish_incoming_drop' "$PLATFORM" 'incoming Wayland DnD missing'
require_source 'wl-copy' "$PLATFORM" 'Wayland clipboard copy bridge missing'
require_source 'wl-paste' "$PLATFORM" 'Wayland clipboard paste bridge missing'
require_source 'WAYLAND_WHEEL_LINE_MULTIPLIER: f32 = 1\.4' "$PLATFORM" '1.4x line wheel contract missing'
require_source 'MouseWheelUnit::Line' "$PLATFORM" 'line wheel unit path missing'
require_source 'MouseWheelUnit::Point' "$PLATFORM" 'point/touchpad wheel path missing'

# Approved v0.8 UI behaviors that must survive the rewrite.
require_source 'show_preferences' "$APP" 'center-pane Preferences state missing'
require_source 'Show the inspector / preview pane' "$APP" 'preview preference missing'
require_source 'window_opacity' "$APP" 'opacity setting missing'
require_source 'icon-grid-scroll' "$APP" 'stable icon-grid scroll path missing'
require_source 'Keep tab activation/context interactions off the dedicated close-button hit box' "$APP" 'dedicated tab-close hit target regression guard missing'
require_source 'full sidebar width' "$APP" 'full-width pinned-row behavior missing'
require_source 'status' "$APP" 'status bar/state integration missing'
require_source 'stable_ui' 'crates/hoard-function/src/theme.rs' 'stable-ui theme integration missing'

# Update/install safety contracts.
require_source 'sha256sum' "$DEPLOY" 'candidate/staged hash verification missing'
require_source 'installed.sha256' "$INSTALLED" 'installed hash evidence missing'
require_source 'runtime-smoke.sh' "$INSTALLED" 'installed runtime smoke missing'
require_source 'inode/directory' "$PROMOTE" 'default-handler promotion contract missing'
require_source 'previous-mime' "$ROLLBACK" 'rollback MIME restoration missing'

BIN="$("$ROOT/scripts/candidate-binary.sh")" || fail 'unable to resolve Hoard candidate binary'
if [[ -x "$BIN" ]]; then
  TMP="$(mktemp -d -t hoard-api3-parity.XXXXXX)"
  trap 'rm -rf "$TMP"' EXIT
  mkdir -p "$TMP/home" "$TMP/config" "$TMP/cache" "$TMP/data" "$TMP/state"
  HOME="$TMP/home" \
  XDG_CONFIG_HOME="$TMP/config" \
  XDG_CACHE_HOME="$TMP/cache" \
  XDG_DATA_HOME="$TMP/data" \
  XDG_STATE_HOME="$TMP/state" \
  "$BIN" --self-test >"$TMP/self-test.log" 2>&1 || {
    cat "$TMP/self-test.log" >&2
    fail 'candidate self-test failed'
  }
  for marker in \
    'PASS  configuration' \
    'PASS  session persistence' \
    'PASS  directory cache' \
    'PASS  copy/move' \
    'PASS  trash/restore' \
    'PASS  undo/redo' \
    'PASS  updater configuration' \
    'PASS  desktop integration'; do
    grep -F "$marker" "$TMP/self-test.log" >/dev/null || fail "self-test marker missing: $marker"
  done
  rm -rf "$TMP"
  trap - EXIT
fi

echo 'HOARD API3 AUTOMATED PARITY PROOF PASSED'
