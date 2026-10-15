#!/usr/bin/env bash
set -euo pipefail
fail() { echo "HOARD NATIVE HOST PREFLIGHT FAILED: $*" >&2; exit 1; }
[[ -n "${WAYLAND_DISPLAY:-}" ]] || fail 'WAYLAND_DISPLAY is not set; Hoard is native Wayland only'
for cmd in pkg-config wl-copy wl-paste ldd xdg-mime; do
  command -v "$cmd" >/dev/null 2>&1 || fail "required native host command missing: $cmd"
done
for pc in wayland-client xkbcommon; do
  pkg-config --exists "$pc" || fail "native build metadata missing: pkg-config $pc"
done
python3 - <<'PY' || fail 'Vulkan runtime loader libvulkan.so.1 could not be loaded'
import ctypes
ctypes.CDLL('libvulkan.so.1')
PY
echo 'HOARD NATIVE HOST PREFLIGHT PASSED'
