use crate::{config, fs_model, identity};
use std::{env, process::Command};

fn command_exists(name: &str) -> bool {
    Command::new("sh")
        .args(["-lc", &format!("command -v {name} >/dev/null 2>&1")])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub fn diagnostics_text() -> String {
    let mut lines = Vec::new();
    lines.push("Hoard diagnostics".to_string());
    lines.push(format!("version: {}", env!("CARGO_PKG_VERSION")));
    lines.push(format!(
        "build-id: {}",
        identity::build_identity().display()
    ));
    let shared = identity::build_identity();
    lines.push(format!(
        "framework: {} API {}",
        shared.framework_version, shared.framework_api
    ));
    lines.push(format!(
        "validator: {} API {}",
        shared.validator_version, shared.validator_api
    ));
    lines.push(format!("os: {}", env::consts::OS));
    lines.push(format!("arch: {}", env::consts::ARCH));
    lines.push(format!(
        "desktop: {}",
        env::var("XDG_CURRENT_DESKTOP").unwrap_or_else(|_| "unknown".into())
    ));
    lines.push(format!(
        "session: {}",
        env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unknown".into())
    ));
    lines.push(format!(
        "hyprland: {}",
        env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
    ));
    lines.push(format!(
        "wayland-display: {}",
        env::var_os("WAYLAND_DISPLAY").is_some()
    ));
    lines.push("platform: native-wayland".into());
    lines.push("window-layer: smithay-client-toolkit".into());
    lines.push("event-loop: calloop".into());
    lines.push("renderer: egui-wgpu".into());
    lines.push("graphics-api: Vulkan".into());
    lines.push("eframe: not used".into());
    lines.push("winit: not used".into());
    lines.push("EGL/GLX: not used".into());
    lines.push("X11/XWayland: not used".into());
    lines.push(format!("config-dir: {}", config::config_dir().display()));
    lines.push(format!("cache-dir: {}", config::cache_dir().display()));
    lines.push(format!("state-dir: {}", config::state_dir().display()));
    lines.push(format!(
        "update-config: {}",
        config::update_config_path().display()
    ));
    lines.push(format!(
        "thumbnail-helper.ffmpegthumbnailer: {}",
        command_exists("ffmpegthumbnailer")
    ));
    lines.push(format!(
        "thumbnail-helper.pdftoppm: {}",
        command_exists("pdftoppm")
    ));
    lines.push(format!(
        "thumbnail-helper.rsvg-convert: {}",
        command_exists("rsvg-convert")
    ));
    lines.push(format!(
        "thumbnail-helper.openscad: {}",
        command_exists("openscad")
    ));
    lines.push(format!("udisksctl: {}", command_exists("udisksctl")));
    lines.push(format!("gio: {}", command_exists("gio")));
    lines.push(format!("wl-copy: {}", command_exists("wl-copy")));
    lines.push(format!("wl-paste: {}", command_exists("wl-paste")));
    lines.push(format!(
        "mounted-user-volumes: {}",
        fs_model::discover_mounts().len()
    ));
    lines.push(format!(
        "unmounted-devices: {}",
        fs_model::discover_unmounted_devices().len()
    ));
    lines
        .push("note: diagnostics intentionally omit private filenames and browsing history".into());
    lines.join("\n") + "\n"
}

pub fn print_diagnostics() {
    print!("{}", diagnostics_text());
}
