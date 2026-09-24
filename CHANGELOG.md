# Changelog

## 0.8.0-dev.7

- Removed the built-in Hoard Image Viewer and the `--image-viewer` runtime mode.
- Image files now follow the normal desktop/default-application path; `Open With…` remains available from file context menus.
- Removed the obsolete “Open images in Hoard's image viewer” preference and default setting. Existing settings files remain compatible; the old key is ignored and disappears the next time settings are saved.
- Extracted the former viewer into the standalone Lattice View codebase for development under the Lattice Desktop project instead of coupling image viewing to the file manager.

## 0.8.0-dev.6

- Added `Open With…` to the file context menu for all non-directory files.
- `Open With…` uses the XDG Desktop Portal application chooser with `ask=true`, allowing the desktop to present registered applications for the selected file type instead of silently launching the default application.
- Kept `Open with Default Application` as a separate image-specific quick action alongside Hoard's internal image viewer.
- Replaced the deprecated egui slider `clamp_to_range` call with the current clamping API.

## 0.8.0-dev.5

- Removed redundant selection-specific Open / Open in New Tab / Open in New Window entries from the global Menu; those actions remain in the selected item/folder context menu.
- Added the standard Cut, Copy, and Paste edit actions to the global Menu, including Ctrl+X/Ctrl+C/Ctrl+V shortcut labels.
- Cut and Copy are disabled when there is no current selection; Paste continues to accept Hoard's internal clipboard or the system file clipboard.
- Preserved the dev.4 toolbar icons, status-bar padding, icon-size slider, translucency, XWayland backend behavior, and inspector layout.

## 0.8.0-dev.4

- Replaced the plain text right-edge toolbar controls with clearer icon-style Menu (☰) and Sort (↕) buttons.
- Expanded the main menu with common file-manager actions such as opening the current folder or current selection in a new tab or new window, creating new items, opening a terminal, and pinning/unpinning the current folder.
- Added a live icon-size slider to the status bar for quick thumbnail scaling without opening Preferences.
- Corrected status-bar padding on all sides so it no longer feels vertically cramped or flush to the borders.
- Preserved the existing translucency, sidebar gutter fix, XWayland backend behavior, and inspector layout from dev.3.

## 0.8.0-dev.3

- Fixes the fully transparent vertical gutter at the right edge of the left sidebar when window translucency is enabled.
- The sidebar SidePanel frame now owns the translucent sidebar surface directly, so scroll-area/frame margins cannot expose raw desktop pixels.
- Keeps child controls transparent only when they inherit an already-painted structural surface; no structural sidebar region is fully transparent.
- Preserves the v0.8.0-dev.2 opacity controls, XWayland/Hyprland compatibility path, toolbar layout, inspector, pinned locations, and drag/drop behavior.

## 0.8.0-dev.2

- Adds a persistent Window Opacity setting in Preferences with live 55–100% adjustment and 85/90/95/100% presets. Structural surfaces become translucent while text, icons, borders, and selection indicators remain crisp.
- Uses eframe's transparent viewport path with a fully transparent clear buffer so Hoard's per-surface alpha can composite against the desktop without requiring a global Hyprland opacity rule.
- Preserves the stable Hyprland XWayland backend and 1:1 UI scaling established in v0.7.5/v0.8.0-dev.1.
- Tightens the one-row toolbar on narrower windows: labels collapse later, search/location widths scale more deliberately, Hidden moves into the overflow menu when space is constrained, and Sort collapses to an icon.
- Gives Pinned locations their own bordered sidebar group with a count, clearer row treatment, and an intentional empty state.
- Adds visible drag/drop targeting in Details, Icons, and sidebar destinations with a green drop outline plus live Move/Copy guidance in the status bar.
- Polishes New -> File / Folder creation with clearer file-type labels, focused naming fields, Enter-to-create, and empty-name validation.
- Removes the image viewer's remaining 25 ms polling repaint; sibling scans and image decoding now wake egui only when their worker results arrive.
- Cleans the four Rust future-compatibility `f32` fallback warnings reported by the v0.8.0-dev.1 build.

## 0.8.0-dev.1

- Starts the v0.8 daily-driver polish cycle from the stable v0.7.5 baseline.
- Replaces Hoard's periodic main-window repaint timer with event-driven wakeups from directory, metadata, file-operation, thumbnail, theme, trash, disk-space, and update workers. Hidden/idle windows no longer redraw just to poll background state.
- Keeps the proven Hyprland XWayland compatibility backend and 1:1 UI scaling unchanged.
- Adds intentional loading, empty-folder, empty-trash, and no-search-results states with useful actions.
- Corrects the status bar to report the real multi-selection count.
- Makes toolbar text labels responsive: configured labels collapse automatically on narrower windows while preserving the location and search fields.
- Reworks Preferences into cleaner Browsing, Interface, Behavior, Desktop compatibility, and Theme sections.
- Preserves the v0.7.5 integrated inspector, pinned-folder workflow, context-menu creation flow, and large-directory responsiveness.

## 0.7.5

- Reworked the right inspector into a full-height integrated pane with a hard divider instead of a floating card separated from the file view.
- Made inspector width responsive to the available content area while preserving a minimum usable file-list width.
- Fixed preview-image distortion by fitting thumbnails to the preview canvas with their original aspect ratio preserved.
- Increased inspector preview resolution to 512 px for cleaner image previews on larger windows.
- Added independent vertical scrolling to the inspector so metadata and actions remain usable on shorter windows.
- Reorganized inspector content into a dedicated preview canvas, wrapped title/type text, a contained Details card, and cleaner Quick Actions.
- Long locations and metadata values now wrap naturally instead of being hard-truncated to 28 characters.
- Corrected folder pinning actions in the inspector so files are no longer offered a misleading Pin Folder action; pinned folders can now be unpinned from the same pane.
- Made the confirmed Hyprland compatibility path permanent using winit 0.30's supported event-loop backend hook to force X11/XWayland without altering `WAYLAND_DISPLAY` for child applications.
- Applies `WINIT_X11_SCALE_FACTOR=1` only during Hoard X11 window creation, then restores the environment so terminals and files opened from Hoard do not inherit its compatibility scaling.
- `--native-wayland`, `--x11`, and `HOARD_BACKEND=...` remain available as explicit backend overrides.

## 0.7.4

- Fixed Hyprland "Application Not Responding" dialogs traced to the native Wayland GL presentation path blocking inside `eglSwapBuffers()` after Hoard is moved to a hidden workspace.
- On Hyprland, Hoard now selects XWayland automatically when `DISPLAY` is available, avoiding the suspended native-Wayland surface path while preserving the same Hoard UI and behavior.
- Added `--native-wayland` to explicitly force native Wayland for testing and future compositor versions.
- Added `--x11` to explicitly force X11/XWayland.
- Added `HOARD_BACKEND=auto|wayland|x11|xwayland` as a persistent backend override; an existing `WINIT_UNIX_BACKEND` setting is also respected when no Hoard-specific override is supplied.
- Kept native Wayland as the automatic path on non-Hyprland Wayland sessions and on Hyprland sessions without XWayland.

## 0.7.3

- Fixed the remaining large-folder UI stall path by replacing repeated full-directory metadata rescans with an indexed O(1) row lookup.
- Added per-frame processing budgets for metadata batches, thumbnail texture uploads, and file-operation progress so background completions cannot monopolize the UI thread.
- Moved Trash counting off the render thread and cached the result instead of rereading the Trash directory every frame.
- Removed per-frame selected-file metadata probes from the inspector; size, modified, and accessed timestamps now reuse asynchronously loaded metadata.
- Moved periodic theme-file mtime checks to a background worker instead of stat'ing the theme on the UI thread.
- Moved restored-tab and typed-path directory validation into the background directory loader so slow, stale, removable, or network paths cannot block initial window creation or navigation.
- Deferred initial mount discovery to the background refresh worker.
- Removed per-frame directory-existence checks from known sidebar/tab folder targets.
- Avoided allocating a full identity index vector on every repaint when filename filtering is empty.
- Preserved the v0.7.2 single-row toolbar, opaque sidebar, pinned-folder layout, and existing visual arrangement.

## 0.7.2

- Rebuilt the file-manager toolbar as one full-width row.
- Removed the sidebar-width toolbar offset and the second toolbar row.
- Kept navigation, location, search, clipboard, view, hidden-file, sort, and menu controls on one line.
- Made the sidebar surface fully opaque regardless of theme alpha.
- Hid the sidebar scroll-bar gutter while retaining mouse-wheel scrolling.
- Removed synchronous disk-capacity probes from the sidebar render path.
- Moved current-folder disk-space probing to a background worker to prevent UI stalls on slow or removable mounts.

## 0.7.1

- Removed New Folder and New Window from the toolbar and tightened the remaining toolbar into cleaner navigation, clipboard, view, hidden-file, sort, and menu groups.
- Added a folder-background context menu with Open in New Window, Open in New Tab, Paste, Refresh, Open in Terminal, and pin/unpin actions.
- Added a nested New menu: File -> Text File, Blank File, Markdown, JSON, Shell Script, Python Script; plus Folder.
- Reworked sidebar spacing, section hierarchy, active-row borders, and pane padding.
- Fixed the translucent/transparent strip above the left pane by carrying the solid sidebar surface through the toolbar region and shipping an opaque sidebar background.

## 0.7.0

- Redesigned the navigation and command toolbars with grouped controls, tighter spacing, clearer selected states, and more consistent icon geometry.
- Added persistent pinned folders and migrated legacy bookmarks automatically.
- Downloads and Screenshots are pinned by default on first v0.7 launch and no longer appear as duplicate normal sidebar entries.
- Added pin/unpin actions to folder context menus and sidebar entries.
- Added typed file icons with compact badges for documents, media, archives, code, configuration, fonts, disk images, applications, and 3D assets.
- Extended visible-item thumbnails beyond images to video, PDF, SVG, and supported 3D model files.
- Added optional runtime thumbnail helpers through the installer: ffmpegthumbnailer, Poppler, librsvg, and OpenSCAD.
- Details view now uses visual-media thumbnails only for visible rows and typed icons for everything else.
- Improved large folder behavior by keeping the existing bounded thumbnail queue and on-screen-only rendering strategy.
- Updated the inspector to use the same typed icons and visual thumbnails as the main views.
