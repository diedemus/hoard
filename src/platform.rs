use std::ffi::OsString;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WindowBackend {
    Auto,
    Wayland,
    X11,
}

pub(crate) fn select_window_backend(args: &[String]) -> WindowBackend {
    if args.iter().any(|a| a == "--native-wayland") {
        return WindowBackend::Wayland;
    }
    if args.iter().any(|a| a == "--x11") {
        return WindowBackend::X11;
    }

    if let Ok(requested) = std::env::var("HOARD_BACKEND") {
        match requested.to_ascii_lowercase().as_str() {
            "wayland" => return WindowBackend::Wayland,
            "x11" | "xwayland" => return WindowBackend::X11,
            "auto" => {}
            _ => eprintln!(
                "Ignoring invalid HOARD_BACKEND={requested:?}; expected auto, wayland, x11, or xwayland"
            ),
        }
    }

    if std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some()
        && std::env::var_os("DISPLAY").is_some()
    {
        WindowBackend::X11
    } else {
        WindowBackend::Auto
    }
}

pub(crate) struct ScaleEnvironmentRestore {
    previous: Option<OsString>,
}

impl ScaleEnvironmentRestore {
    pub(crate) fn restore(self) {
        match self.previous {
            Some(value) => std::env::set_var("WINIT_X11_SCALE_FACTOR", value),
            None => std::env::remove_var("WINIT_X11_SCALE_FACTOR"),
        }
    }
}

pub(crate) fn configure_native_options(
    options: &mut eframe::NativeOptions,
    backend: WindowBackend,
) -> Option<ScaleEnvironmentRestore> {
    match backend {
        WindowBackend::X11 => {
            options.event_loop_builder = Some(Box::new(|builder| {
                use winit::platform::x11::EventLoopBuilderExtX11 as _;
                builder.with_x11();
            }));

            // XWayland reported a larger logical DPI than Hoard's native
            // Wayland layout on the target Hyprland setup. Apply the proven
            // 1:1 scale only while eframe creates Hoard's window, then restore
            // the process environment so child apps do not inherit this tweak.
            if std::env::var_os("WINIT_X11_SCALE_FACTOR").is_none() {
                let previous = std::env::var_os("WINIT_X11_SCALE_FACTOR");
                std::env::set_var("WINIT_X11_SCALE_FACTOR", "1");
                Some(ScaleEnvironmentRestore { previous })
            } else {
                None
            }
        }
        WindowBackend::Wayland => {
            options.event_loop_builder = Some(Box::new(|builder| {
                use winit::platform::wayland::EventLoopBuilderExtWayland as _;
                builder.with_wayland();
            }));
            None
        }
        WindowBackend::Auto => None,
    }
}
