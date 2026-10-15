# Contributing to Hoard

Hoard targets Linux Wayland desktops and keeps a deliberately small product-specific build surface.

## Development workflow

1. Work from a clean source tree.
2. Keep generic build, dependency-resolution, ShellCheck, Cargo, artifact, lifecycle, and rollback behavior in the canonical `stable-build-framework`; Hoard should contain only declarative policy and Hoard-specific hooks.
3. Run the complete local gate with:

   ```bash
   ./framework/host-gate
   ```

4. Treat the host gate as authoritative for dependency resolution, Rust/Cargo, native Wayland/Vulkan runtime behavior, clipboard/DnD, release packaging, linkage, deployment, and installed-runtime verification.

## Product constraints

Preserve Hoard's native Wayland/Vulkan architecture and approved v0.8 desktop design unless a change is intentionally scoped to those areas. Avoid introducing eframe/winit/EGL/X11/XWayland compatibility paths.

Keep generated `.framework/`, `Cargo.lock`, `target/`, `dist/`, logs, and local credentials out of commits.
