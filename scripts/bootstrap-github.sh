#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

command -v git >/dev/null || { echo "git is required." >&2; exit 1; }
command -v gh >/dev/null || { echo "GitHub CLI is required: sudo dnf install gh" >&2; exit 1; }

gh auth status >/dev/null 2>&1 || {
  echo "GitHub CLI is not authenticated. Run: gh auth login" >&2
  exit 1
}

if ! git config user.name >/dev/null || ! git config user.email >/dev/null; then
  echo "Git identity is not configured. Set git config --global user.name and user.email first." >&2
  exit 1
fi

if ! git rev-parse --is-inside-work-tree >/dev/null 2>&1; then
  git init -b main
fi

# Hoard is an application: commit Cargo.lock for reproducible GitHub builds.
command -v cargo >/dev/null || { echo "cargo is required to generate Cargo.lock." >&2; exit 1; }
cargo generate-lockfile

git add .
if ! git rev-parse --verify HEAD >/dev/null 2>&1; then
  git commit -m "Initial Hoard import"
elif ! git diff --cached --quiet; then
  git commit -m "Update Hoard source"
fi

if ! git remote get-url origin >/dev/null 2>&1; then
  gh repo create hoard \
    --public \
    --description "A Wayland-first, deeply themeable Rust file manager" \
    --source=. \
    --remote=origin \
    --push
else
  git push -u origin main
fi

REPO="$(gh repo view --json nameWithOwner --jq .nameWithOwner)"
echo "GitHub repository: https://github.com/$REPO"

echo "Configuring installed Hoard to update from GitHub Releases..."
./build-install.sh --github-repo "$REPO" --enable-auto-update

echo
echo "Next release workflow:"
echo "  1. bump Cargo.toml version"
echo "  2. commit + push"
echo "  3. git tag vVERSION && git push origin vVERSION"
echo "GitHub Actions will build and publish the release assets automatically."
