# Hoard 0.8.0-rc.2 UI preservation contract

The rc.2 migration changes the Linux platform/runtime layer, not Hoard's approved user interface.

## Preserved from 0.8.0-rc.1

- Existing egui application/view code and theme model.
- Compact dark visual language, translucent structural surfaces, crisp borders and selection.
- One-row main toolbar and its responsive collapse behavior.
- Tab bar, tab interactions, per-tab state and navigation history.
- Left sidebar structure, pinned locations, devices, network shortcuts and non-transparent gutter treatment.
- Details and icon views, selection behavior, sort controls, search/location fields and context menus.
- Full-height right inspector/preview pane, its spacing and metadata/actions layout.
- Bottom status bar, icon-size control, progress/operation feedback and spacing.
- Preferences organization and existing settings semantics.
- Existing dialogs, file-operation collision handling, Trash/restore and Undo/Redo UI.
- Configurable opacity and theme surface alpha behavior.

## Intentional app.rs integration changes

Only platform-bound seams were changed in `app.rs`: construction no longer depends on `eframe::CreationContext`; window move/minimize/maximize/close are emitted as platform commands; the same internal drag payload also arms native Wayland outgoing DnD; and the old `eframe::App` callback is exposed as the native runtime's `update_ui` method.

No panel hierarchy, approved dimensions, theme metrics, file-list layout or visual redesign was introduced by the platform migration.

## Native runtime target

`egui -> egui-wgpu -> wgpu/Vulkan -> native Wayland`, with SCTK/wayland-client owning the window, seats/input, xdg-shell lifecycle and data-device DnD.

The release gate rejects eframe, winit, EGL, GLX, X11 and XWayland runtime dependencies before installation.
