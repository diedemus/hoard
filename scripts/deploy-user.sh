#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"; cd "$ROOT"
VERSION="$(python3 - <<'PYI'
import tomllib
with open('framework/project.toml','rb') as f: print(tomllib.load(f)['project']['version'])
PYI
)"
REVISION="${PROJECT_CANDIDATE_REVISION:-$(python3 - <<'PYI'
import tomllib
with open('framework/project.toml','rb') as f: print(tomllib.load(f)['project']['candidate_revision'])
PYI
)}"
EXPECTED_VERSION="hoard $VERSION"; EXPECTED_BUILD="hoard $VERSION host=$REVISION"; PREFIX="${PREFIX:-$HOME/.local}"; STATE="$ROOT/.framework/hoard-deploy-state"
[[ -d "$STATE" ]] || { echo 'pre-deploy state missing' >&2; exit 1; }
BIN="$("$ROOT/scripts/candidate-binary.sh")"
[[ "$("$BIN" --version)" == "$EXPECTED_VERSION" ]] || { echo 'candidate version mismatch' >&2; exit 1; }
[[ "$("$BIN" --build-id)" == "$EXPECTED_BUILD" ]] || { echo 'candidate build-id mismatch' >&2; exit 1; }
BINDIR="$PREFIX/bin"; APPDIR="$PREFIX/share/applications"; ICONDIR="$PREFIX/share/icons/hicolor/scalable/apps"; SHAREDIR="$PREFIX/share/hoard"; CONFDIR="$HOME/.config/hoard"; BUILDDIR="$PREFIX/share/hoard/build-info"
mkdir -p "$BINDIR" "$APPDIR" "$ICONDIR" "$SHAREDIR/themes" "$CONFDIR/themes" "$BUILDDIR"
STAGED="$BINDIR/.hoard.${REVISION}.new"; install -m 0755 "$BIN" "$STAGED"
CANDIDATE_SHA="$(sha256sum "$BIN" | awk '{print $1}')"; STAGED_SHA="$(sha256sum "$STAGED" | awk '{print $1}')"; [[ "$CANDIDATE_SHA" == "$STAGED_SHA" ]] || { rm -f "$STAGED"; echo 'staged/candidate hash mismatch' >&2; exit 1; }; [[ "$("$STAGED" --build-id)" == "$EXPECTED_BUILD" ]] || { rm -f "$STAGED"; echo 'staged identity mismatch' >&2; exit 1; }; mv -f "$STAGED" "$BINDIR/hoard"
install -m 0644 assets/hoard.svg "$ICONDIR/hoard.svg"; install -m 0644 config/themes/hoard.toml "$SHAREDIR/themes/hoard.toml"; [[ -e "$CONFDIR/themes/hoard.toml" ]] || install -m 0644 config/themes/hoard.toml "$CONFDIR/themes/hoard.toml"; [[ -e "$CONFDIR/settings.toml" ]] || install -m 0644 config/settings.toml "$CONFDIR/settings.toml"; [[ -e "$CONFDIR/update.toml" ]] || install -m 0644 config/update.toml.example "$CONFDIR/update.toml"; sed "s#@BINDIR@#$BINDIR#g" assets/hoard.desktop.in > "$APPDIR/hoard.desktop"; chmod 0644 "$APPDIR/hoard.desktop"
install -m 0644 .framework/framework-lock.json "$BUILDDIR/framework-lock.json"; install -m 0644 Cargo.lock "$BUILDDIR/Cargo.lock"; [[ -f .framework/evidence.json ]] && install -m 0644 .framework/evidence.json "$BUILDDIR/evidence.json" || true
printf '%s  %s\n' "$CANDIDATE_SHA" "$BIN" > "$STATE/candidate.sha256"; printf '%s\n' "$EXPECTED_BUILD" > "$STATE/expected-build"
echo "HOARD DEPLOY STAGE PASSED: $BINDIR/hoard"
