#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PREFIX="${PREFIX:-$HOME/.local}"
SET_DEFAULT=0
INSTALL_DEPS=0
ENABLE_UPDATES=0
GITHUB_REPO="diedemus/hoard"
LEGACY_UPDATE_URL=""

usage() {
  cat <<USAGE
Usage: ./build-install.sh [options]

Options:
  --deps                       Install Fedora/Nobara build dependencies with dnf
  --set-default                Set Hoard as inode/directory handler
  --enable-auto-update         Install and enable a daily user systemd update timer
  --github-repo OWNER/REPO     GitHub repository used for release updates
  --update-url URL             Legacy v0.3 manifest updater compatibility
  --prefix PATH                Install prefix (default: ~/.local)
  -h, --help                   Show this help

Hoard uses https://github.com/diedemus/hoard as its canonical update source.
Pass --github-repo OWNER/REPO only for forks or alternate release channels.

Recommended:
  ./build-install.sh --deps --set-default --enable-auto-update
USAGE
}

while (($#)); do
  case "$1" in
    --deps) INSTALL_DEPS=1 ;;
    --set-default) SET_DEFAULT=1 ;;
    --enable-auto-update) ENABLE_UPDATES=1 ;;
    --github-repo) shift; GITHUB_REPO="${1:-}" ;;
    --update-url) shift; LEGACY_UPDATE_URL="${1:-}" ;;
    --prefix) shift; PREFIX="${1:-$PREFIX}" ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown option: $1" >&2; usage; exit 2 ;;
  esac
  shift
done

normalize_repo_from_url() {
  local url="$1" repo=""
  case "$url" in
    https://github.com/*) repo="${url#https://github.com/}" ;;
    http://github.com/*) repo="${url#http://github.com/}" ;;
    git@github.com:*) repo="${url#git@github.com:}" ;;
    ssh://git@github.com/*) repo="${url#ssh://git@github.com/}" ;;
  esac
  repo="${repo%.git}"
  repo="${repo%/}"
  if [[ "$repo" =~ ^[^/]+/[^/]+$ ]]; then printf '%s\n' "$repo"; fi
}

detect_github_repo() {
  local remote="" repo="" login=""
  if command -v git >/dev/null 2>&1; then
    remote="$(git -C "$ROOT" remote get-url origin 2>/dev/null || true)"
    repo="$(normalize_repo_from_url "$remote")"
    if [[ -n "$repo" ]]; then printf '%s\n' "$repo"; return 0; fi
  fi
  if command -v gh >/dev/null 2>&1 && gh auth status >/dev/null 2>&1; then
    login="$(gh api user --jq .login 2>/dev/null || true)"
    if [[ -n "$login" ]]; then printf '%s/hoard\n' "$login"; return 0; fi
  fi
  return 1
}

if (( INSTALL_DEPS )); then
  sudo dnf install -y \
    rust cargo gcc gcc-c++ make pkgconf-pkg-config \
    wayland-devel libxkbcommon-devel mesa-libGL-devel \
    libX11-devel libXcursor-devel libXi-devel libXrandr-devel libxcb-devel \
    openssl-devel dbus-devel zstd git gh wl-clipboard

  # Optional runtime thumbnail helpers. Hoard falls back to typed icons if a
  # helper is not available, so an unavailable package must not block install.
  sudo dnf install -y ffmpegthumbnailer poppler-utils librsvg2-tools openscad || \
    echo "Warning: one or more optional thumbnail helpers could not be installed; typed icons will be used instead." >&2
fi

command -v cargo >/dev/null || { echo "cargo is required. Re-run with --deps or install Rust." >&2; exit 1; }

cd "$ROOT"

if [[ -z "$GITHUB_REPO" ]]; then
  GITHUB_REPO="$(detect_github_repo || true)"
fi

if [[ -n "$GITHUB_REPO" && ! "$GITHUB_REPO" =~ ^[^/]+/[^/]+$ ]]; then
  echo "ERROR: --github-repo must be OWNER/REPO, got: $GITHUB_REPO" >&2
  exit 1
fi

if [[ -n "$GITHUB_REPO" ]]; then
  echo "==> GitHub update repository: $GITHUB_REPO"
else
  echo "==> GitHub update repository not detected yet; update config will remain disabled."
fi

echo "==> Building Hoard"
if [[ -n "$GITHUB_REPO" ]]; then
  HOARD_GITHUB_REPOSITORY="$GITHUB_REPO" cargo build --release --locked 2>/dev/null || \
  HOARD_GITHUB_REPOSITORY="$GITHUB_REPO" cargo build --release
else
  cargo build --release --locked 2>/dev/null || cargo build --release
fi

BINDIR="$PREFIX/bin"
APPDIR="$PREFIX/share/applications"
ICONDIR="$PREFIX/share/icons/hicolor/scalable/apps"
SHAREDIR="$PREFIX/share/hoard"
CONFDIR="$HOME/.config/hoard"

mkdir -p "$BINDIR" "$APPDIR" "$ICONDIR" "$SHAREDIR/themes" "$CONFDIR/themes"

rm -f "$BINDIR/tiamat-files" "$APPDIR/tiamat-files.desktop" 2>/dev/null || true

install -m 0755 target/release/hoard "$BINDIR/hoard"
install -m 0644 assets/hoard.svg "$ICONDIR/hoard.svg"
install -m 0644 config/themes/hoard.toml "$SHAREDIR/themes/hoard.toml"

if [[ ! -e "$CONFDIR/themes/hoard.toml" ]]; then
  install -m 0644 config/themes/hoard.toml "$CONFDIR/themes/hoard.toml"
fi

sed "s#@BINDIR@#$BINDIR#g" assets/hoard.desktop.in > "$APPDIR/hoard.desktop"
chmod 0644 "$APPDIR/hoard.desktop"

if [[ ! -e "$CONFDIR/settings.toml" ]]; then
  cp config/settings.toml "$CONFDIR/settings.toml"
fi

if [[ -n "$LEGACY_UPDATE_URL" ]]; then
  cat > "$CONFDIR/update.toml" <<CFG
enabled = true
provider = "manifest"
repository = ""
manifest_url = "$LEGACY_UPDATE_URL"
channel = "stable"
check_interval_hours = 24
CFG
elif [[ -n "$GITHUB_REPO" ]]; then
  cat > "$CONFDIR/update.toml" <<CFG
enabled = true
provider = "github"
repository = "$GITHUB_REPO"
manifest_url = ""
channel = "stable"
check_interval_hours = 24
CFG
elif [[ ! -e "$CONFDIR/update.toml" ]]; then
  cp config/update.toml.example "$CONFDIR/update.toml"
fi

if (( SET_DEFAULT )); then
  xdg-mime default hoard.desktop inode/directory
fi

if (( ENABLE_UPDATES )); then
  if [[ -z "$GITHUB_REPO" && -z "$LEGACY_UPDATE_URL" ]]; then
    echo "ERROR: automatic updates need a GitHub repository. Create the repo first or pass --github-repo OWNER/hoard." >&2
    exit 1
  fi
  mkdir -p "$HOME/.config/systemd/user"
  cat > "$HOME/.config/systemd/user/hoard-update.service" <<SERVICE
[Unit]
Description=Update Hoard from GitHub Releases
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=/bin/bash -lc '$BINDIR/hoard --update || true'
SERVICE

  cat > "$HOME/.config/systemd/user/hoard-update.timer" <<'TIMER'
[Unit]
Description=Daily Hoard GitHub release update check

[Timer]
OnBootSec=15min
OnUnitActiveSec=24h
RandomizedDelaySec=45min
Persistent=true

[Install]
WantedBy=timers.target
TIMER
  systemctl --user daemon-reload
  systemctl --user enable --now hoard-update.timer
fi

command -v update-desktop-database >/dev/null && update-desktop-database "$APPDIR" >/dev/null 2>&1 || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -f -t "$PREFIX/share/icons/hicolor" >/dev/null 2>&1 || true

echo
echo "Installed: $BINDIR/hoard"
echo "Desktop:   $APPDIR/hoard.desktop"
echo "Theme:     $CONFDIR/themes/hoard.toml"
echo "Updates:   $CONFDIR/update.toml"
if [[ -n "$GITHUB_REPO" ]]; then echo "GitHub:    https://github.com/$GITHUB_REPO"; fi
if (( SET_DEFAULT )); then echo "Default directory handler: Hoard"; fi
if (( ENABLE_UPDATES )); then echo "Automatic GitHub release updates: enabled"; fi
echo
echo "Launch with: hoard"
