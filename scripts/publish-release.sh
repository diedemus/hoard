#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v gh >/dev/null || { echo "GitHub CLI (gh) is required." >&2; exit 1; }
gh auth status >/dev/null

VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)"
TAG="v$VERSION"
REPO="$(gh repo view --json nameWithOwner --jq .nameWithOwner)"

if ! git diff --quiet || ! git diff --cached --quiet; then
  echo "ERROR: commit your changes before publishing a release." >&2
  exit 1
fi

./scripts/make-release.sh

mapfile -t ASSETS < <(find dist -maxdepth 1 -type f \( -name "hoard-${VERSION}-*.tar.zst" -o -name "hoard-${VERSION}-*.tar.zst.sha256" \) | sort)
((${#ASSETS[@]} >= 2)) || { echo "Release assets were not created." >&2; exit 1; }

if ! git rev-parse "$TAG" >/dev/null 2>&1; then
  git tag -a "$TAG" -m "Hoard $VERSION"
fi
git push origin main
git push origin "$TAG"

if gh release view "$TAG" -R "$REPO" >/dev/null 2>&1; then
  gh release upload "$TAG" "${ASSETS[@]}" -R "$REPO" --clobber
else
  gh release create "$TAG" "${ASSETS[@]}" -R "$REPO" --title "Hoard $VERSION" --generate-notes
fi

echo "Published Hoard $VERSION to https://github.com/$REPO/releases/tag/$TAG"
