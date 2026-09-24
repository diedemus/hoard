<div align="center">
  <img src="assets/hoard.svg" width="96" alt="Hoard logo">

# Hoard

**A fast, polished Rust file manager for Linux, built for modern Wayland/Hyprland desktops.**

`v0.8.0-dev.6` · Rust · eframe/egui · MIT
</div>

---

## Overview

Hoard is a Linux file manager written in Rust with a compact desktop-oriented interface, asynchronous media browsing, tabs, a built-in inspector, configurable translucency, pinned locations, and native file-management workflows.

The current `0.8.0` development line focuses on daily-driver polish and responsiveness. Hoard supports both Wayland and X11/XWayland. On Hyprland, Hoard automatically prefers XWayland when available because a native Wayland/EGL presentation stall was reproduced when hidden workspaces suspended the window surface. Native Wayland remains available explicitly for testing.

## Highlights

- Details and icon/thumbnail views
- Fast asynchronous browsing for large media directories
- Visible-item thumbnail loading with bounded background work
- Image, video, PDF, SVG, and supported 3D-model previews when helper tools are available
- Tabs with restore, duplicate, reorder, reopen, and close actions
- Open folders in a new tab or a separate Hoard window
- Back, forward, up, refresh, editable location, and search/filter controls
- Cut, copy, paste, and Linux desktop clipboard integration
- Drag and drop between folders, tabs, and sidebar destinations
- Highlighted drop targets with move/copy feedback
- Persistent pinned folders, with Downloads and Screenshots pinned by default when present
- Mounted-device and network shortcuts
- Integrated inspector with aspect-correct previews, wrapped metadata, and quick actions
- Trash, restore, permanent delete, and empty-trash management
- Context menus for normal file-manager operations
- `New` workflows for files and folders
- Sort controls and folders-first ordering
- Status-bar icon-size control
- Configurable window translucency while keeping text and controls readable
- Hidden-file toggle
- Persistent preferences and tab/session state
- TOML themes
- Event-driven UI wakeups instead of an idle repaint timer
- GitHub Releases updater with SHA-256 verification
- Optional daily user-level systemd update timer

## Current version

This repository currently tracks:

```text
Hoard 0.8.0-dev.6
```

This is a development release. The `0.8.0` line is being refined toward a stable daily-driver release.

## Install on Fedora / Nobara

Clone the repository and run the installer:

```bash
git clone https://github.com/diedemus/hoard.git
cd hoard
./build-install.sh --deps --set-default --enable-auto-update
```

The installer can:

- install required Fedora/Nobara build dependencies;
- compile an optimized release binary;
- install Hoard under `~/.local`;
- install the desktop file and icon;
- optionally make Hoard the default directory handler;
- optionally enable the daily GitHub Release updater.

### Build without changing desktop defaults

```bash
./build-install.sh --deps
```

### Build manually

```bash
cargo build --release
./target/release/hoard
```

## Usage

Launch Hoard:

```bash
hoard
```

Open a specific directory:

```bash
hoard ~/Downloads
```

Show the installed version:

```bash
hoard --version
```

### Window backend

Hoard chooses an appropriate backend automatically.

Force native Wayland:

```bash
hoard --native-wayland
```

Force X11/XWayland:

```bash
hoard --x11
```

You can also use a persistent environment override:

```bash
HOARD_BACKEND=wayland hoard
HOARD_BACKEND=x11 hoard
```

### Hyprland note

On Hyprland, Hoard currently prefers XWayland when available. This avoids a confirmed native-Wayland Mesa/EGL presentation stall that could trigger an "Application Not Responding" dialog after the window was left on a hidden workspace.

The XWayland path uses a 1:1 application scale so Hoard keeps the intended layout proportions. Native Wayland is still available with `--native-wayland` for regression testing as compositor/graphics-stack behavior changes.

## Updates

Check whether a newer GitHub Release is available:

```bash
hoard --update-check
```

Install the latest configured release:

```bash
hoard --update
```

Check the automatic updater:

```bash
systemctl --user status hoard-update.timer
```

The update source is stored in:

```text
~/.config/hoard/update.toml
```

## Configuration

Hoard stores user configuration under:

```text
~/.config/hoard/
```

Important files include:

```text
~/.config/hoard/settings.toml
~/.config/hoard/themes/hoard.toml
~/.config/hoard/update.toml
~/.config/hoard/pinned.txt
```

Edit the active theme directly with:

```bash
${EDITOR:-nano} ~/.config/hoard/themes/hoard.toml
```

Hoard can reload the theme from its application menu.

## Optional thumbnail helpers

Hoard falls back to typed file icons when a helper is unavailable. On Fedora/Nobara, the installer attempts to install optional helpers including:

- `ffmpegthumbnailer` for video thumbnails
- `poppler-utils` for PDF previews
- `librsvg2-tools` for SVG previews
- `openscad` for supported 3D preview workflows

Missing optional helpers do not prevent Hoard from running.

## Desktop integration

Set Hoard as the default directory handler:

```bash
xdg-mime default hoard.desktop inode/directory
```

Verify it:

```bash
xdg-mime query default inode/directory
```

## Repository layout

```text
assets/                 Application icon and desktop entry template
config/                 Default settings, update config, and themes
docs/                   Reference material
scripts/                Release, publishing, bootstrap, and uninstall helpers
src/                    Rust source
.github/workflows/      GitHub Actions release workflow
build-install.sh        Fedora/Nobara build and install helper
Cargo.toml              Rust package definition
```

## Releases

Pushing a version tag matching `Cargo.toml` triggers the GitHub Actions release workflow:

```bash
git tag -a v0.8.0-dev.6 -m "Hoard 0.8.0-dev.6"
git push origin v0.8.0-dev.6
```

The workflow builds the Linux x86_64 release bundle and its SHA-256 checksum and publishes them to GitHub Releases.

Release bundles are created as:

```text
hoard-<version>-x86_64-unknown-linux-gnu.tar.zst
hoard-<version>-x86_64-unknown-linux-gnu.tar.zst.sha256
```

## Development

Recommended local checks before pushing:

```bash
cargo fmt --check
cargo check
cargo build --release
```

Run Hoard directly from the release build:

```bash
./target/release/hoard
```

## License

Hoard is licensed under the [MIT License](LICENSE).
