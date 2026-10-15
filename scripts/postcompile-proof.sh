#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
fail(){ echo "HOARD POSTCOMPILE PROOF FAILED: $*" >&2; exit 1; }
"$ROOT/scripts/native-runtime-regression-check.sh"
BIN="$("$ROOT/scripts/candidate-binary.sh")"
VERSION="$(python3 - <<'PYI'
import tomllib
with open('framework/project.toml','rb') as f: print(tomllib.load(f)['project']['version'])
PYI
)"
REVISION="${PROJECT_CANDIDATE_REVISION:-api3-r25}"
[[ "$("$BIN" --version)" == "hoard $VERSION" ]] || fail 'candidate version identity mismatch'
[[ "$("$BIN" --build-id)" == "hoard $VERSION host=$REVISION" ]] || fail 'candidate build identity mismatch'
TREE="$(cargo tree --locked -e normal)"
if grep -E '(^|[[:space:]├└│─])(eframe|egui-winit|winit|glutin|glow|khronos-egl|x11|x11rb)[[:space:]]+v' >/dev/null <<<"$TREE"; then fail 'forbidden desktop dependency resolved'; fi
FEATURES="$(cargo tree --locked -e features)"
if grep -Eq 'wgpu feature "(gles|webgl)"|wayland-sys feature "egl"|egui-wgpu feature "(winit|x11)"' <<<"$FEATURES"; then fail 'forbidden graphics/window feature resolved'; fi
if ldd "$BIN" | grep -Ei 'libEGL|libGLX|libX11|libXcb' >/dev/null; then fail 'forbidden GL/X11 runtime linkage found'; fi
"$ROOT/scripts/api3-parity-proof.sh"
echo 'HOARD POSTCOMPILE PROOF PASSED'
