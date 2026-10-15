#!/usr/bin/env bash
set -euo pipefail
ROOT="${STABLE_BUILD_FRAMEWORK_CANDIDATE_ROOT:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}"
cd "$ROOT"
fail() { echo "HOARD NATIVE RUNTIME REGRESSION FAILED: $*" >&2; exit 1; }
PLATFORM="apps/hoard/src/platform.rs"
APP="crates/hoard-function/src/app.rs"

[[ -f Cargo.toml && -f Cargo.lock && -f "$PLATFORM" && -f framework/dependency-policy.toml ]] || fail "native/API3 source missing"
grep -F 'version = "0.8.0-dev.1"' framework/project.toml >/dev/null || fail "project version mismatch"
if grep -RIE --include='Cargo.toml' '^[[:space:]]*(eframe|egui-winit|winit|glow|glutin|khronos-egl|x11|x11rb|x11-dl|wayland-client)[[:space:]]*=' Cargo.toml; then
  fail "forbidden desktop/protocol dependency declared directly"
fi

require_native() { grep -Eq "$1" "$PLATFORM" || fail "$2"; }
require_app() { grep -Eq "$1" "$APP" || fail "$2"; }
grep -F 'option_env!("PROJECT_CANDIDATE_REVISION")' crates/hoard-core/src/identity.rs >/dev/null || fail "compile-time candidate identity adapter missing"
require_native 'reexports::.*client|client::\{' "Wayland client must be owned through SCTK reexports"
require_native 'Backends::VULKAN' "runtime does not force Vulkan"
require_native 'Backend::Vulkan' "runtime Vulkan backend verification missing"
require_native 'Connection::connect_to_env' "direct Wayland connection missing"
require_native 'XdgShell::bind' "XDG window path missing"
require_native 'WaylandSource::new' "calloop Wayland source missing"
require_native 'Renderer::new' "direct egui-wgpu renderer missing"
require_native 'InstanceDescriptor::new_without_display_handle' "current wgpu instance API missing"
require_native 'apply_limit_buckets:[[:space:]]*false' "current wgpu adapter options missing"
require_native 'SurfaceColorSpace::Auto' "surface color-space contract missing"
require_native 'CurrentSurfaceTexture::Success' "surface acquire contract missing"
require_native 'queue[[:space:]]*\.present\(frame\)' "wgpu presentation contract missing"
require_native 'multiview_mask:[[:space:]]*None' "render-pass multiview contract missing"
require_native 'depth_slice:[[:space:]]*None' "render attachment depth-slice contract missing"

require_native 'set_request_repaint_callback' "egui repaint callback missing"
require_native 'repaint_deadline' "deadline-based repaint scheduling missing"
require_native 'next_dispatch_timeout' "adaptive event-loop pacing missing"
require_native 'take_due_repaint' "delayed repaint wakeup missing"
require_native 'clear_repaint_deadline' "repaint deadline clearing missing"
require_native 'state\.focused \|\| startup_active' "focused/startup active repaint scheduling missing"
require_native 'Duration::from_millis\(16\)' "focused native frame cadence missing"
require_native 'let active = state\.focused \|\| startup_active' "focused native event-pump path missing"
require_native 'if startup_active \|\|' "bounded startup repaint heartbeat missing"
require_native 'if self\.smoke_mode' "deterministic native smoke pacing missing"
require_native 'run_ui\(raw_input' "current egui run_ui path missing"
require_native 'Event::ModifiersChanged' "modifier changes not forwarded"
require_native 'for \(id, deltas\) in &full_output\.textures_delta\.set' "texture deltas must be borrowed"
require_native 'for id in &full_output\.textures_delta\.free' "texture frees must be borrowed"
require_native 'PointerButton::Extra1' "back/side mouse button mapping missing"
require_native 'PointerButton::Extra2' "forward/extra mouse button mapping missing"
require_app 'button_pressed\(egui::PointerButton::Extra1\)' "back mouse button is not consumed by navigation"
require_app 'button_pressed\(egui::PointerButton::Extra2\)' "forward mouse button is not consumed by navigation"
require_app '\(alt && left\) \|\| mouse_back' "back mouse button navigation route missing"
require_app '\(alt && right\) \|\| mouse_forward' "forward mouse button navigation route missing"
require_native 'Event::MouseWheel' "native wheel translation missing"
require_native 'WAYLAND_WHEEL_LINE_MULTIPLIER:[[:space:]]*f32[[:space:]]*=[[:space:]]*1\.4' "shared 1.4x wheel line multiplier missing"
require_native 'MouseWheelUnit::Line' "notched/high-resolution wheel line-unit path missing"
require_native 'value120' "high-resolution Wayland wheel metadata path missing"
require_native 'MouseWheelUnit::Point' "continuous touchpad point-unit path missing"
require_app 'vertical_align\(egui::Align::Center\)' "single-line editable text vertical centering missing"
require_app 'Margin::symmetric\(8,[[:space:]]*4\)' "single-line editable text padding contract missing"
require_app 'ui\.set_min_width\(\(row_width - 18\.0\)\.max\(1\.0\)\)' "full-width pinned navigation-row contract missing"
require_app 'let footer_rect = Rect::from_min_max' "icon-view metadata footer contract missing"
require_app 'let filename_rect = Rect::from_min_max' "icon-view filename footer row missing"
require_app 'let type_icon = if entry.file_type == "Image"' "icon-view type icon contract missing"
require_app 'filename_rect\.intersect\(grid_clip\)' "icon-view filename clip intersection missing"
require_app 'with_clip_rect' "icon-view clipped painter path missing"
require_app 'let visual_slot = Rect::from_min_max' "icon-view visual-slot separation missing"
require_app 'let thumb_frame = visual_slot.shrink2' "thumbnail inner-frame contract missing"
require_app 'let row_gap = 24\.0_f32' "fixed vertical grid pitch missing"
require_app 'let card_pad_x = 12\.0_f32' "icon-card internal padding missing"
grep -F 'let grid_clip = selection_viewport.intersect(content_rect);' crates/hoard-function/src/app.rs >/dev/null || fail 'directory grid clip missing'
require_app 'thumb_frame\.intersect\(grid_clip\)' "thumbnail/icon pane clipping missing"
require_app 'ui\.set_clip_rect\(content_rect\)' "directory-pane hard clipping missing"
require_app '\.show\(ui, \|ui\|' "single-coordinate scroll canvas missing"
require_app 'let row_stride = card_h \+ row_gap' "fixed icon-grid row stride missing"
require_app 'let icon_viewport = ui\.clip_rect\(\)' "outer icon-pane viewport contract missing"
require_app 'let selection_viewport = icon_viewport\.intersect\(ui\.clip_rect\(\)\)' "full icon-pane selection viewport contract missing"
require_app 'let grid_pad_x = 18\.0_f32' "center-pane horizontal selection gutter contract missing"
require_app 'let grid_pad_y = 16\.0_f32' "center-pane vertical selection gutter contract missing"
require_app 'let content_pad = 12\.0_f32' "center-pane internal padding missing"
require_app 'col as f32 \* \(card_w \+ col_gap\)' "fixed icon-grid column pitch missing"
require_native 'Event::WindowFocused\(true\)' "focus gain forwarding missing"
require_native 'Event::WindowFocused\(false\)' "focus loss forwarding missing"
require_native 'ensure_seat_object\(qh, &seat\)' "capability-order-independent seat binding missing"
require_native 'HOARD_NATIVE keyboard-bound' "keyboard binding trace missing"
require_native 'HOARD_NATIVE themed-pointer-bound' "themed pointer binding trace missing"
require_native 'set_buffer_scale\(self\.scale_factor\)' "Wayland buffer scale propagation missing"
require_native 'wl-copy' "Wayland clipboard copy bridge missing"
require_native 'wl-paste' "Wayland clipboard paste bridge missing"
require_native 'Keysym::V \| Keysym::v => Key::V' "modifier-safe V key mapping missing"
require_native 'Keysym::X \| Keysym::x => Key::X' "modifier-safe X key mapping missing"
require_native 'Keysym::C \| Keysym::c => Key::C' "modifier-safe C key mapping missing"
require_native 'key == Some\(Key::V\)' "paste shortcut must use resolved key rather than modifier-suppressed UTF-8 text"
require_native 'text/plain;charset=utf-8' "UTF-8 clipboard MIME fallback missing"
require_native 'Some\("text/plain"\)' "plain-text clipboard MIME fallback missing"
require_native 'DataDeviceManagerState::bind' "native Wayland data-device/DnD path missing"
require_native 'create_drag_and_drop_source' "outgoing native file DnD missing"
require_native 'finish_incoming_drop' "incoming native file DnD missing"
require_native 'HOARD_NATIVE_SMOKE PASS' "runtime smoke marker missing"
require_app 'runtime_smoke_ready' "application smoke readiness contract missing"

if sed -n '/let raw_input = RawInput {/,/};/p' "$PLATFORM" | grep -Eq '^[[:space:]]*modifiers:'; then
  fail "RawInput contains removed egui modifiers field"
fi
if grep -RInE '&&[[:space:]]*let |if let .*&&|while let .*&&' crates apps --include='*.rs' > /tmp/hoard-let-chain.$$; then
  cat /tmp/hoard-let-chain.$$ >&2
  rm -f /tmp/hoard-let-chain.$$
  fail "unexpected let-chain syntax found in reused parity source"
fi
rm -f /tmp/hoard-let-chain.$$

# Native text clipboard integration must use egui's dedicated copy/cut events.
grep -Fq 'self.input_events.push(Event::Copy);' apps/hoard/src/platform.rs || { echo 'missing native egui copy event' >&2; exit 1; }
grep -Fq 'self.input_events.push(Event::Cut);' apps/hoard/src/platform.rs || { echo 'missing native egui cut event' >&2; exit 1; }
grep -Fq 'self.input_events.push(Event::Paste(text));' apps/hoard/src/platform.rs || { echo 'missing native egui paste event' >&2; exit 1; }

echo "HOARD NATIVE RUNTIME REGRESSION PASSED"
