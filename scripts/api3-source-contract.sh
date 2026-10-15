#!/usr/bin/env bash
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"
fail(){ echo "HOARD API3 SOURCE CONTRACT FAILED: $*" >&2; exit 1; }
[[ "$(tr -d '[:space:]' < framework/required-api)" == 3 ]] || fail 'framework API must be 3'
[[ "$(tr -d '[:space:]' < validator/required-api)" == 1 ]] || fail 'validator API must be 1'
grep -F 'git+https://github.com/diedemus/stable-build-framework.git#v0.5.3' framework/canonical-source >/dev/null || fail 'framework release pin mismatch'
grep -F 'tag = "v0.5.3"' Cargo.toml >/dev/null || fail 'shared core Git release pin missing'
grep -F 'egui.workspace = true' crates/hoard-core/Cargo.toml >/dev/null || fail 'hoard-core uses egui thumbnail types without direct egui dependency'
grep -F 'name = "stable-app-core"' Cargo.lock >/dev/null || fail 'stable-app-core missing from committed lock'
grep -F 'name = "stable-ui"' Cargo.lock >/dev/null || fail 'stable-ui missing from committed lock'
grep -F '0e2a886faf30bd1660c233fb4d4d9b0b161bf2f3' Cargo.lock >/dev/null || fail 'shared core lock provenance mismatch'
for legacy in framework/validation.toml .adaptive-validation.toml release-gate.sh build.sh build-install.sh src; do
  [[ ! -e "$legacy" ]] || fail "obsolete API2/root architecture remains: $legacy"
done
for required in crates/hoard-core/src/lib.rs crates/hoard-function/src/lib.rs apps/hoard/src/main.rs apps/hoard/src/platform.rs docs/API3-RC-PARITY.md scripts/api3-parity-proof.sh; do
  [[ -f "$required" ]] || fail "rewrite source missing: $required"
done
if grep -RIE --include='Cargo.toml' '^[[:space:]]*(eframe|egui-winit|winit|glow|glutin|khronos-egl|x11|x11rb|x11-dl|wayland-client)[[:space:]]*=' .; then fail 'forbidden desktop/protocol dependency declared directly'; fi
# Dependency/backend ownership is proven from manifests and the resolved Cargo graph.
# Do not reject diagnostic/help text that merely names forbidden legacy backends.
if grep -RInE --include='*.rs' '(^|[^[:alnum:]_])(eframe|egui_winit|glutin|glow)::' crates apps; then
  fail 'forbidden legacy backend crate referenced by product Rust source'
fi
grep -F 'stable_ui' crates/hoard-function/src/theme.rs >/dev/null || fail 'stable-ui theme consumption missing'
grep -F 'stable_app_core::BuildIdentity' crates/hoard-core/src/identity.rs >/dev/null || fail 'stable-app-core identity consumption missing'
grep -F 'WAYLAND_WHEEL_LINE_MULTIPLIER: f32 = 1.4' apps/hoard/src/platform.rs >/dev/null || fail '1.4x line-wheel contract missing'
grep -F 'MouseWheelUnit::Point' apps/hoard/src/platform.rs >/dev/null || fail 'unscaled point-unit touchpad path missing'
grep -F 'let compact = total < 1400.0;' crates/hoard-function/src/app.rs >/dev/null || fail 'responsive toolbar collapse threshold missing'
grep -F 'let fixed_commands = if labels' crates/hoard-function/src/app.rs >/dev/null || fail 'responsive toolbar command budget missing'
! grep -F 'ToolIcon::Cut' crates/hoard-function/src/app.rs >/dev/null || fail 'cut must not be in the top toolbar'
! grep -F 'ToolIcon::Copy' crates/hoard-function/src/app.rs >/dev/null || fail 'copy must not be in the top toolbar'
! grep -F 'ToolIcon::Paste' crates/hoard-function/src/app.rs >/dev/null || fail 'paste must not be in the top toolbar'
! grep -F 'ui.checkbox(&mut self.show_hidden, "Hidden")' crates/hoard-function/src/app.rs >/dev/null || fail 'hidden-files checkbox must not be in the toolbar'
grep -F 'ui.checkbox(&mut self.show_hidden, "Show hidden files")' crates/hoard-function/src/app.rs >/dev/null || fail 'hidden-files preference missing'
grep -F 'Keysym::V | Keysym::v => Key::V' apps/hoard/src/platform.rs >/dev/null || fail 'modifier-safe V key mapping missing'
grep -F 'Keysym::X | Keysym::x => Key::X' apps/hoard/src/platform.rs >/dev/null || fail 'modifier-safe X key mapping missing'
grep -F 'Keysym::C | Keysym::c => Key::C' apps/hoard/src/platform.rs >/dev/null || fail 'modifier-safe C key mapping missing'
grep -F 'pub struct FolderStats' crates/hoard-core/src/fs_model.rs >/dev/null || fail 'folder inspector statistics model missing'
grep -F 'pub fn folder_stats(path: &Path, cancel: &AtomicBool)' crates/hoard-core/src/fs_model.rs >/dev/null || fail 'folder inspector statistics worker missing'
grep -F '"Files"' crates/hoard-function/src/app.rs >/dev/null || fail 'folder file-count inspector field missing'
grep -F '"Space on disk"' crates/hoard-function/src/app.rs >/dev/null || fail 'folder space-on-disk inspector field missing'
grep -F '"Volume"' crates/hoard-function/src/app.rs >/dev/null || fail 'folder volume-space inspector field missing'
grep -F 'fn select_all_visible(&mut self)' crates/hoard-function/src/app.rs >/dev/null || fail 'Ctrl+A visible-selection helper missing'
grep -F 'struct RubberBandSelection' crates/hoard-function/src/app.rs >/dev/null || fail 'rubber-band selection state missing'
grep -F 'apply_rubber_band_selection' crates/hoard-function/src/app.rs >/dev/null || fail 'rubber-band selection application missing'
grep -F 'fn selection_context_menu(&mut self, ui: &mut egui::Ui)' crates/hoard-function/src/app.rs >/dev/null || fail 'selection context-menu helper missing'
grep -F 'fn restore_selected_from_trash(&mut self)' crates/hoard-function/src/app.rs >/dev/null || fail 'multi-selection Trash restore helper missing'
grep -F 'QueuedOperation::Restore(paths)' crates/hoard-function/src/app.rs >/dev/null || fail 'multi-selection Trash restore operation missing'
grep -F 'ui.button("Invert Selection")' crates/hoard-function/src/app.rs >/dev/null || fail 'invert-selection context action missing'
grep -F 'ui.button("Clear Selection")' crates/hoard-function/src/app.rs >/dev/null || fail 'clear-selection context action missing'
grep -F 'let col_gap = 22.0_f32;' crates/hoard-function/src/app.rs >/dev/null || fail 'icon-grid selection whitespace regression'
grep -F 'let grid_pad_x = 18.0_f32;' crates/hoard-function/src/app.rs >/dev/null || fail 'icon-grid outer selection gutter regression'
grep -F 'if ctrl && a && !text_focus' crates/hoard-function/src/app.rs >/dev/null || fail 'Ctrl+A shortcut missing'
grep -F 'i.pointer.primary_pressed()' crates/hoard-function/src/app.rs >/dev/null || fail 'empty-space drag-selection press detection missing'
grep -F 'painter.rect_stroke(' crates/hoard-function/src/app.rs >/dev/null || fail 'rubber-band visual missing'
grep -F 'let content_height = grid_height.max(viewport_height);' crates/hoard-function/src/app.rs >/dev/null || fail 'full center-pane icon selection surface missing'
grep -F 'let viewport_width = ui.available_width();' crates/hoard-function/src/app.rs >/dev/null || fail 'outer icon-pane width capture missing'
grep -F 'let icon_viewport = ui.clip_rect();' crates/hoard-function/src/app.rs >/dev/null || fail 'outer icon-pane viewport capture missing'
grep -F 'let available = viewport_width.max(card_w + grid_pad_x * 2.0);' crates/hoard-function/src/app.rs >/dev/null || fail 'full icon-pane selection width missing'
grep -F 'self.theme.accent_soft.linear_multiply(0.28)' crates/hoard-function/src/app.rs >/dev/null || fail 'translucent icon rubber-band fill missing'
grep -F 'id_salt("details-scroll")' crates/hoard-function/src/app.rs >/dev/null || fail 'details selection surface missing'
grep -F 'fn columns_context_menu(&mut self, ui: &mut egui::Ui)' crates/hoard-function/src/app.rs >/dev/null || fail 'details Columns context menu missing'
grep -F 'fn sort_context_menu(&mut self, ui: &mut egui::Ui)' crates/hoard-function/src/app.rs >/dev/null || fail 'Sort context menu missing'
! grep -F 'ToolIcon::Sort' crates/hoard-function/src/app.rs >/dev/null || fail 'toolbar Sort button/icon must be removed'
grep -F 'SortColumn::Created' crates/hoard-core/src/fs_model.rs >/dev/null || fail 'Created sort column missing'
grep -F 'SortColumn::Owner' crates/hoard-core/src/fs_model.rs >/dev/null || fail 'Owner sort column missing'
grep -F 'SortColumn::Permissions' crates/hoard-core/src/fs_model.rs >/dev/null || fail 'Permissions sort column missing'
grep -F 'fn apply_theme_preset_to(mut theme: Theme, preset: &str) -> Theme' crates/hoard-function/src/app.rs >/dev/null || fail 'packaged theme preset implementation missing'
for preset in   obsidian-violet ocean-teal amber-graphite cobalt-night ember-stone   forest-carbon plum-rose ivory-ink solar-paper nord-aurora; do
  grep -F "\"$preset\"" crates/hoard-function/src/app.rs >/dev/null || fail "Tiamat palette missing: $preset"
done
grep -F 'pub theme_preset: String' crates/hoard-core/src/config.rs >/dev/null || fail 'theme preset persistence missing'
grep -F '.intersect(selection_viewport);' crates/hoard-function/src/app.rs >/dev/null || fail 'full icon rubber-band viewport clipping missing'
grep -F 'fn chrome_icon_color(theme: &Theme, active: bool) -> Color32' crates/hoard-function/src/app.rs >/dev/null || fail 'theme-aware chrome icon policy missing'
grep -F 'header_response.context_menu(|ui| self.columns_context_menu(ui));' crates/hoard-function/src/app.rs >/dev/null || fail 'full details-header Columns context menu missing'
grep -F 'response.context_menu(|ui| self.columns_context_menu(ui));' crates/hoard-function/src/app.rs >/dev/null || fail 'details header-cell Columns context menu missing'
grep -F 'grip.context_menu(|ui| self.columns_context_menu(ui));' crates/hoard-function/src/app.rs >/dev/null || fail 'details header-grip Columns context menu missing'
[[ "$(grep -Fc 'Reload Theme' crates/hoard-function/src/app.rs)" -eq 1 ]] || fail 'Reload Theme must exist only in Preferences'
grep -F 'get_pointer_with_theme' apps/hoard/src/platform.rs >/dev/null || fail 'native themed pointer integration missing'
grep -F 'output.cursor_icon' apps/hoard/src/platform.rs >/dev/null || fail 'egui cursor output is not forwarded to Wayland'
echo 'HOARD API3 SOURCE CONTRACT PASSED'
