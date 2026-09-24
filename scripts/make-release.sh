#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

normalize_repo_from_url() {
  local url="$1" repo=""
  case "$url" in
    https://github.com/*) repo="${url#https://github.com/}" ;;
    git@github.com:*) repo="${url#git@github.com:}" ;;
    ssh://git@github.com/*) repo="${url#ssh://git@github.com/}" ;;
  esac
  repo="${repo%.git}"; repo="${repo%/}"
  [[ "$repo" =~ ^[^/]+/[^/]+$ ]] && printf '%s\n' "$repo"
}

REPO="${HOARD_GITHUB_REPOSITORY:-}"
if [[ -z "$REPO" ]]; then
  REMOTE="$(git remote get-url origin 2>/dev/null || true)"
  REPO="$(normalize_repo_from_url "$REMOTE")"
fi

if [[ -n "$REPO" ]]; then
  echo "Embedding GitHub repository: $REPO"
  HOARD_GITHUB_REPOSITORY="$REPO" cargo build --release --locked 2>/dev/null || \
  HOARD_GITHUB_REPOSITORY="$REPO" cargo build --release
else
  echo "WARNING: no GitHub repository detected; release binary will require repository in update.toml" >&2
  cargo build --release --locked 2>/dev/null || cargo build --release
fi

VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)"
ARCH="$(uname -m)"
case "$ARCH" in
  x86_64) TARGET="x86_64-unknown-linux-gnu" ;;
  aarch64) TARGET="aarch64-unknown-linux-gnu" ;;
  *) echo "Unsupported release arch: $ARCH" >&2; exit 1 ;;
esac

OUT="$ROOT/dist"
PAYLOAD="$OUT/hoard-release"
rm -rf "$PAYLOAD"
mkdir -p "$PAYLOAD/bin" "$PAYLOAD/share/icons/hicolor/scalable/apps" "$PAYLOAD/share/hoard/themes" "$PAYLOAD/share/hoard/config"
install -m 0755 target/release/hoard "$PAYLOAD/bin/hoard"
install -m 0644 assets/hoard.svg "$PAYLOAD/share/icons/hicolor/scalable/apps/hoard.svg"
install -m 0644 config/themes/hoard.toml "$PAYLOAD/share/hoard/themes/hoard.toml"
install -m 0644 config/settings.toml "$PAYLOAD/share/hoard/config/settings.toml"

mkdir -p "$OUT"
BUNDLE="$OUT/hoard-${VERSION}-${TARGET}.tar.zst"
rm -f "$BUNDLE" "$BUNDLE.sha256"
tar -C "$OUT" -cf - hoard-release | zstd -19 -T0 -o "$BUNDLE"
(
  cd "$OUT"
  sha256sum "$(basename "$BUNDLE")" > "$(basename "$BUNDLE").sha256"
)

echo "Bundle:   $BUNDLE"
echo "Checksum: $BUNDLE.sha256"
echo "GitHub release tag should be: v$VERSION"
