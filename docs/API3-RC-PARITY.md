# Hoard API-3 RC parity gate

The API-3 line must not replace the mature v0.8 RC until every required capability is host-validated.

## Core file management
- [ ] filesystem navigation and directory listing
- [ ] tabs and new tabs/windows
- [ ] cut/copy/paste
- [ ] rename/move/copy
- [ ] trash/restore/permanent delete
- [ ] open/open-with and context menus
- [ ] sorting/view controls
- [ ] pinned locations/sidebar
- [ ] standard multi-selection: Ctrl+A, Ctrl-click toggle, Shift-click range, empty-space clear, icon-grid rubber-band selection, Select All/Invert/Clear context actions

## Native Wayland
- [ ] direct SCTK/calloop path
- [ ] egui + egui-wgpu
- [ ] Vulkan-only wgpu
- [ ] no eframe/winit/egui-winit/glutin/Glow/EGL/GLES/OpenGL/X11/XWayland product backend
- [ ] native clipboard
- [ ] native incoming DnD
- [ ] native outgoing DnD

## UI
- [ ] approved compact dark Hoard v0.8 presentation
- [ ] one-row responsive toolbar
- [ ] crisp borders / controlled translucency
- [ ] approved directory/card spacing and metadata footer
- [ ] full-width pinned rows
- [ ] preview pane
- [ ] center-pane Preferences with live changes
- [ ] editable-field alignment/padding
- [ ] status-bar spacing and icon-size slider
- [ ] fixed left scroll gutter
- [ ] opacity setting and context-menu parity
- [ ] common palette/glyph/decor metrics sourced from stable-ui

## Input
- [ ] Line wheel units x1.4
- [ ] Point/touchpad units unscaled

## Update/install safety
- [ ] GitHub release/update model and SHA-256 verification
- [ ] candidate/staged/installed hash equality
- [ ] installed self-test and native runtime smoke
- [ ] inode/directory remains unchanged until installed validation passes
- [ ] rollback restores previous binary and previous handler

No unchecked item may be waived for RC promotion.

## Automated evidence added in api3-r8

`./framework/host-gate` now runs `scripts/api3-parity-proof.sh` after release compilation. It fail-closes on the presence of the major file-manager, native Wayland/Vulkan, input, UI-preservation, update/install-safety contracts and runs the candidate's isolated `--self-test` when the release binary is present.

This automated proof is evidence, not a waiver of the checklist above. Visual/interactive items still require manual host acceptance before RC promotion, especially clipboard/DnD interaction, exact UI presentation, tab hit behavior, preview/Preferences interaction, and wheel/touchpad feel.
