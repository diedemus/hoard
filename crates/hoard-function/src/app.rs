use crate::{PlatformCommand, theme::Theme};
use egui::{
    self, Align, Align2, Color32, FontId, Layout, Pos2, Rect, RichText, Sense, Stroke,
    TextureHandle, Vec2,
};
use hoard_core::{
    cache,
    config::{self, AppSettings, UpdateConfig},
    diagnostics,
    fs_model::{self, FileEntry, SortColumn},
    history::{Signature, UndoAction, UndoHistory},
    search::{self, SearchFilter},
    thumbnail, updater,
};
use stable_ui::{Glyph, GlyphPrimitive, WindowDecorations, glyph_primitives};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    io::Write,
    os::fd::AsFd,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
    time::{Duration, Instant, SystemTime},
};
use url::Url;

type SpaceResult = (PathBuf, Option<(u64, u64)>);
type FolderStatsResult = (PathBuf, Option<fs_model::FolderStats>);

#[derive(Clone)]
struct Tab {
    path: PathBuf,
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    search: String,
    selected: Option<PathBuf>,
    selected_paths: Vec<PathBuf>,
    view: ViewMode,
    scroll_y: f32,
}

impl Tab {
    fn new(path: PathBuf, view: ViewMode) -> Self {
        Self {
            path,
            back: Vec::new(),
            forward: Vec::new(),
            search: String::new(),
            selected: None,
            selected_paths: Vec::new(),
            view,
            scroll_y: 0.0,
        }
    }
    fn label(&self) -> String {
        if self.path == dirs::home_dir().unwrap_or_default() {
            return "Home".into();
        }
        if self.path == fs_model::trash_files_dir() {
            return "Trash".into();
        }
        self.path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("/")
            .to_string()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Details,
    Icons,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DetailColumn {
    Name,
    Type,
    Size,
    Modified,
    Created,
    Owner,
    Permissions,
}

impl DetailColumn {
    const ALL: [Self; 7] = [
        Self::Name,
        Self::Type,
        Self::Size,
        Self::Modified,
        Self::Created,
        Self::Owner,
        Self::Permissions,
    ];

    fn key(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Type => "type",
            Self::Size => "size",
            Self::Modified => "modified",
            Self::Created => "created",
            Self::Owner => "owner",
            Self::Permissions => "permissions",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Type => "Type",
            Self::Size => "Size",
            Self::Modified => "Modified",
            Self::Created => "Created",
            Self::Owner => "Owner",
            Self::Permissions => "Permissions",
        }
    }

    fn default_width(self) -> f32 {
        match self {
            Self::Name => 320.0,
            Self::Type => 150.0,
            Self::Size => 110.0,
            Self::Modified | Self::Created => 190.0,
            Self::Owner => 110.0,
            Self::Permissions => 120.0,
        }
    }

    fn min_width(self) -> f32 {
        match self {
            Self::Name => 180.0,
            Self::Type => 110.0,
            Self::Size => 92.0,
            Self::Modified | Self::Created => 150.0,
            Self::Owner => 100.0,
            Self::Permissions => 110.0,
        }
    }

    fn sort_column(self) -> SortColumn {
        match self {
            Self::Name => SortColumn::Name,
            Self::Type => SortColumn::Type,
            Self::Size => SortColumn::Size,
            Self::Modified => SortColumn::Modified,
            Self::Created => SortColumn::Created,
            Self::Owner => SortColumn::Owner,
            Self::Permissions => SortColumn::Permissions,
        }
    }
}

#[derive(Clone, Copy)]
enum ToolIcon {
    Back,
    Forward,
    Up,
    Refresh,
    Home,
    Details,
    Icons,
    Search,
    Menu,
}

#[derive(Clone, Copy)]
enum ClipboardMode {
    Copy,
    Move,
}

#[derive(Clone)]
struct ClipboardItem {
    paths: Vec<PathBuf>,
    mode: ClipboardMode,
}

#[derive(Clone, Debug)]
struct DragPayload {
    paths: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
struct RubberBandSelection {
    start: Pos2,
    current: Pos2,
    additive: bool,
    base: HashSet<PathBuf>,
}

#[derive(Clone, Copy, Debug)]
struct RubberBandGridLayout {
    content_rect: Rect,
    item_count: usize,
    columns: usize,
    card_size: Vec2,
    col_gap: f32,
    row_gap: f32,
    padding: Vec2,
}

#[derive(Clone)]
struct QueuedFileOperation {
    paths: Vec<PathBuf>,
    dest: PathBuf,
    mode: ClipboardMode,
    collision: fs_model::CollisionPolicy,
}

#[derive(Clone)]
enum QueuedOperation {
    Transfer(QueuedFileOperation),
    Trash(Vec<PathBuf>),
    Restore(Vec<PathBuf>),
    Rename { src: PathBuf, new_name: String },
    CreateFolder { parent: PathBuf, name: String },
    CreateFile { parent: PathBuf, name: String },
    Duplicate(PathBuf),
    DeletePermanent(Vec<PathBuf>),
    EmptyTrash,
}

#[derive(Clone)]
struct PendingCollision {
    paths: Vec<PathBuf>,
    dest: PathBuf,
    mode: ClipboardMode,
    conflicts: usize,
}

enum FileOpMessage {
    Progress {
        done: usize,
        total: usize,
        name: String,
        bytes_done: u64,
        total_bytes: Option<u64>,
    },
    Done {
        succeeded: usize,
        failed: usize,
        moved: bool,
        created: Vec<(PathBuf, Signature)>,
        moved_pairs: Vec<(PathBuf, PathBuf)>,
        errors: Vec<String>,
        cancelled: bool,
        undoable: bool,
    },
    SimpleDone {
        status: String,
        undo: Option<UndoAction>,
        errors: Vec<String>,
        select: Option<PathBuf>,
        destructive: bool,
    },
}

enum RecursiveSearchMessage {
    Done {
        root: PathBuf,
        key: String,
        entries: Vec<FileEntry>,
    },
}

enum MetadataMessage {
    Batch(PathBuf, Vec<fs_model::EntryMetadata>),
    Done(PathBuf),
}

struct DirectoryLoadResult {
    path: PathBuf,
    entries: Vec<FileEntry>,
    mounts: Vec<(String, PathBuf)>,
    unmounted_devices: Vec<(String, PathBuf)>,
    error: Option<String>,
}

pub struct HoardApp {
    theme: Theme,
    egui_ctx: egui::Context,
    theme_mtime: Option<SystemTime>,
    theme_check_rx: Receiver<Option<SystemTime>>,
    settings: AppSettings,
    update_config: UpdateConfig,
    tabs: Vec<Tab>,
    active_tab: usize,
    entries: Vec<FileEntry>,
    entry_index: HashMap<PathBuf, usize>,
    selected: Option<PathBuf>,
    selected_paths: HashSet<PathBuf>,
    search: String,
    address: String,
    show_hidden: bool,
    sort: SortColumn,
    sort_ascending: bool,
    view: ViewMode,
    dirty: bool,
    pinned: Vec<PathBuf>,
    mounts: Vec<(String, PathBuf)>,
    unmounted_devices: Vec<(String, PathBuf)>,
    network_history: Vec<String>,
    status: String,
    update_rx: Option<Receiver<String>>,
    open_with_rx: Option<Receiver<String>>,
    maximized: bool,
    thumbnails: HashMap<thumbnail::ThumbnailKey, TextureHandle>,
    thumbnail_pending: HashSet<thumbnail::ThumbnailKey>,
    thumbnail_failed: HashSet<thumbnail::ThumbnailKey>,
    thumbnail_loader: thumbnail::ThumbnailLoader,
    directory_rx: Option<Receiver<DirectoryLoadResult>>,
    loading: bool,
    space_rx: Option<Receiver<SpaceResult>>,
    current_space: Option<(u64, u64)>,
    folder_stats_rx: Option<Receiver<FolderStatsResult>>,
    folder_stats_cancel: Option<Arc<AtomicBool>>,
    folder_stats_path: Option<PathBuf>,
    folder_stats: Option<fs_model::FolderStats>,
    trash_count: usize,
    trash_count_rx: Receiver<usize>,
    metadata_rx: Option<Receiver<MetadataMessage>>,
    metadata_loading: bool,
    file_op_rx: Option<Receiver<FileOpMessage>>,
    file_op_busy: bool,
    file_op_queue: VecDeque<QueuedOperation>,
    file_op_cancel: Option<Arc<AtomicBool>>,
    file_op_progress: Option<(usize, usize, String, u64, Option<u64>)>,
    pending_collision: Option<PendingCollision>,
    operation_errors: Vec<String>,
    undo_history: UndoHistory,
    drag_payload: Option<DragPayload>,
    closed_tabs: Vec<Tab>,
    show_preferences: bool,
    clipboard: Option<ClipboardItem>,
    rename_target: Option<PathBuf>,
    rename_text: String,
    show_new_folder: bool,
    new_folder_name: String,
    show_new_file: bool,
    new_file_name: String,
    delete_targets: Vec<PathBuf>,
    empty_trash_confirm: bool,
    selection_anchor: Option<PathBuf>,
    rubber_band: Option<RubberBandSelection>,
    location_focus_requested: bool,
    recursive_search: bool,
    recursive_search_rx: Option<Receiver<RecursiveSearchMessage>>,
    recursive_search_cancel: Option<Arc<AtomicBool>>,
    recursive_search_key: String,
    recursive_searching: bool,
    search_type_filter: String,
    search_min_size_mb: String,
    search_max_age_days: String,
    cache_hits: u64,
    cache_misses: u64,
    last_directory_signature: Option<(u64, u64)>,
    next_directory_watch: Instant,
    platform_commands: VecDeque<PlatformCommand>,
}

impl HoardApp {
    /// Shared single-line field geometry: readable left inset and vertically
    /// centered text regardless of the containing toolbar/dialog row height.
    fn singleline_text_edit<'a>(text: &'a mut String) -> egui::TextEdit<'a> {
        egui::TextEdit::singleline(text)
            .vertical_align(egui::Align::Center)
            .margin(egui::Margin::symmetric(8, 4))
    }

    fn darken_surface(color: Color32, factor: f32) -> Color32 {
        let factor = factor.clamp(0.0, 1.0);
        Color32::from_rgba_unmultiplied(
            (color.r() as f32 * factor).round() as u8,
            (color.g() as f32 * factor).round() as u8,
            (color.b() as f32 * factor).round() as u8,
            color.a(),
        )
    }

    fn chrome_icon_color(theme: &Theme, active: bool) -> Color32 {
        if active {
            theme.accent
        } else {
            theme.foreground
        }
    }

    fn detail_ellipsize(
        painter: &egui::Painter,
        text: &str,
        font: &FontId,
        color: Color32,
        max_width: f32,
    ) -> String {
        if text.is_empty() || max_width <= 0.0 {
            return String::new();
        }

        let measure = |value: &str| {
            painter
                .layout_no_wrap(value.to_owned(), font.clone(), color)
                .size()
                .x
        };

        if measure(text) <= max_width {
            return text.to_owned();
        }

        let ellipsis = "…";
        if measure(ellipsis) > max_width {
            return String::new();
        }

        let chars: Vec<char> = text.chars().collect();
        let mut lo = 0usize;
        let mut hi = chars.len();

        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            let mut candidate: String = chars[..mid].iter().collect();
            candidate.push('…');

            if measure(&candidate) <= max_width {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }

        let mut out: String = chars[..lo].iter().collect();
        out.push('…');
        out
    }

    fn paint_detail_text(
        ui: &egui::Ui,
        cell: Rect,
        left_inset: f32,
        right_inset: f32,
        text: &str,
        font: FontId,
        color: Color32,
    ) {
        let text_rect = Rect::from_min_max(
            Pos2::new(cell.left() + left_inset, cell.top()),
            Pos2::new(
                (cell.right() - right_inset).max(cell.left() + left_inset),
                cell.bottom(),
            ),
        );

        if text_rect.width() <= 0.0 {
            return;
        }

        let display = Self::detail_ellipsize(ui.painter(), text, &font, color, text_rect.width());

        ui.painter().with_clip_rect(cell).text(
            Pos2::new(text_rect.left(), cell.center().y),
            Align2::LEFT_CENTER,
            display,
            font,
            color,
        );
    }

    fn paint_sort_indicator(painter: &egui::Painter, cell: Rect, ascending: bool, color: Color32) {
        let cx = cell.right() - 13.0;
        let cy = cell.center().y;

        let points = if ascending {
            vec![
                Pos2::new(cx - 4.0, cy + 2.5),
                Pos2::new(cx, cy - 2.5),
                Pos2::new(cx + 4.0, cy + 2.5),
            ]
        } else {
            vec![
                Pos2::new(cx - 4.0, cy - 2.5),
                Pos2::new(cx, cy + 2.5),
                Pos2::new(cx + 4.0, cy - 2.5),
            ]
        };

        painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
    }

    pub fn new(ctx: &egui::Context, start: Option<PathBuf>) -> Self {
        let mut settings = config::load_settings();
        settings.window_opacity = settings.window_opacity.clamp(0.55, 1.0);
        let theme =
            Self::apply_theme_preset_to(Theme::load(&config::theme_path()), &settings.theme_preset)
                .with_surface_opacity(settings.window_opacity);
        theme.apply_visuals(ctx);

        fs_model::ensure_trash_dirs();
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let view = if settings.default_view.eq_ignore_ascii_case("icons") {
            ViewMode::Icons
        } else {
            ViewMode::Details
        };
        let sort = match settings.sort_column.to_ascii_lowercase().as_str() {
            "type" => SortColumn::Type,
            "size" => SortColumn::Size,
            "modified" => SortColumn::Modified,
            "created" => SortColumn::Created,
            "owner" => SortColumn::Owner,
            "permissions" => SortColumn::Permissions,
            _ => SortColumn::Name,
        };
        let session = if settings.remember_tabs {
            config::load_session()
        } else {
            config::SessionState::default()
        };
        let mut tabs = if let Some(start) = start {
            vec![Tab::new(start, view)]
        } else if settings.remember_tabs && !session.tab_state.is_empty() {
            session
                .tab_state
                .iter()
                .take(24)
                .filter_map(|saved| {
                    if saved.path.is_empty() {
                        return None;
                    }
                    let mut tab = Tab::new(
                        PathBuf::from(&saved.path),
                        if saved.view.eq_ignore_ascii_case("icons") {
                            ViewMode::Icons
                        } else {
                            ViewMode::Details
                        },
                    );
                    tab.back = saved.back.iter().map(PathBuf::from).collect();
                    tab.forward = saved.forward.iter().map(PathBuf::from).collect();
                    tab.search = saved.search.clone();
                    tab.selected_paths = saved.selected.iter().map(PathBuf::from).collect();
                    tab.selected = tab.selected_paths.first().cloned();
                    tab.scroll_y = saved.scroll_y.max(0.0);
                    Some(tab)
                })
                .collect::<Vec<_>>()
        } else if settings.remember_tabs {
            session
                .tabs
                .iter()
                .take(24)
                .map(PathBuf::from)
                .map(|p| Tab::new(p, view))
                .collect::<Vec<_>>()
        } else {
            vec![Tab::new(home.clone(), view)]
        };
        if tabs.is_empty() {
            tabs.push(Tab::new(home.clone(), view));
        }
        let restored_active = if settings.remember_tabs {
            session.active_tab.min(tabs.len().saturating_sub(1))
        } else {
            0
        };

        let egui_ctx = ctx.clone();
        let initial_theme_mtime = std::fs::metadata(config::theme_path())
            .and_then(|m| m.modified())
            .ok();

        let (theme_watch_tx, theme_watch_rx) = mpsc::channel();
        {
            let repaint = egui_ctx.clone();
            let mut last = initial_theme_mtime;
            std::thread::spawn(move || {
                loop {
                    std::thread::sleep(Duration::from_secs(2));
                    let mt = std::fs::metadata(config::theme_path())
                        .and_then(|m| m.modified())
                        .ok();
                    if mt != last {
                        last = mt;
                        if theme_watch_tx.send(mt).is_err() {
                            break;
                        }
                        repaint.request_repaint();
                    }
                }
            });
        }

        let (trash_watch_tx, trash_watch_rx) = mpsc::channel();
        {
            let repaint = egui_ctx.clone();
            std::thread::spawn(move || {
                let mut last = None;
                loop {
                    let count = fs_model::trash_count();
                    if last != Some(count) {
                        last = Some(count);
                        if trash_watch_tx.send(count).is_err() {
                            break;
                        }
                        repaint.request_repaint();
                    }
                    std::thread::sleep(Duration::from_secs(3));
                }
            });
        }

        let initial_selected = tabs.get(restored_active).and_then(|t| t.selected.clone());
        let initial_selected_paths: HashSet<PathBuf> = tabs
            .get(restored_active)
            .map(|t| t.selected_paths.iter().cloned().collect())
            .unwrap_or_default();
        let initial_search = tabs
            .get(restored_active)
            .map(|t| t.search.clone())
            .unwrap_or_default();
        let initial_view = tabs.get(restored_active).map(|t| t.view).unwrap_or(view);

        let mut app = Self {
            theme,
            egui_ctx: egui_ctx.clone(),
            theme_mtime: initial_theme_mtime,
            theme_check_rx: theme_watch_rx,
            settings: settings.clone(),
            update_config: config::load_update_config(),
            tabs,
            active_tab: restored_active,
            entries: Vec::new(),
            entry_index: HashMap::new(),
            selected: initial_selected,
            selected_paths: initial_selected_paths,
            search: initial_search,
            address: String::new(),
            show_hidden: settings.show_hidden,
            sort,
            sort_ascending: settings.sort_ascending,
            view: initial_view,
            dirty: true,
            pinned: config::load_pinned(),
            mounts: Vec::new(),
            unmounted_devices: Vec::new(),
            network_history: config::load_network_history(),
            status: String::new(),
            update_rx: None,
            open_with_rx: None,
            maximized: false,
            thumbnails: HashMap::new(),
            thumbnail_pending: HashSet::new(),
            thumbnail_failed: HashSet::new(),
            thumbnail_loader: thumbnail::ThumbnailLoader::new(4, egui_ctx.clone()),
            directory_rx: None,
            loading: false,
            space_rx: None,
            current_space: None,
            folder_stats_rx: None,
            folder_stats_cancel: None,
            folder_stats_path: None,
            folder_stats: None,
            trash_count: 0,
            trash_count_rx: trash_watch_rx,
            metadata_rx: None,
            metadata_loading: false,
            file_op_rx: None,
            file_op_busy: false,
            file_op_queue: VecDeque::new(),
            file_op_cancel: None,
            file_op_progress: None,
            pending_collision: None,
            operation_errors: Vec::new(),
            undo_history: UndoHistory::new(64),
            drag_payload: None,
            closed_tabs: Vec::new(),
            show_preferences: false,
            clipboard: None,
            rename_target: None,
            rename_text: String::new(),
            show_new_folder: false,
            new_folder_name: String::new(),
            show_new_file: false,
            new_file_name: String::new(),
            delete_targets: Vec::new(),
            empty_trash_confirm: false,
            selection_anchor: None,
            rubber_band: None,
            location_focus_requested: false,
            recursive_search: settings.recursive_search_default,
            recursive_search_rx: None,
            recursive_search_cancel: None,
            recursive_search_key: String::new(),
            recursive_searching: false,
            search_type_filter: String::new(),
            search_min_size_mb: String::new(),
            search_max_age_days: String::new(),
            cache_hits: 0,
            cache_misses: 0,
            last_directory_signature: None,
            next_directory_watch: Instant::now() + Duration::from_secs(2),
            platform_commands: VecDeque::new(),
        };
        app.sync_address();
        if app.settings.thumbnail_disk_cache {
            let limit = app.settings.thumbnail_cache_limit_mb;
            let age = app.settings.thumbnail_cache_max_age_days;
            std::thread::spawn(move || {
                let _ = cache::prune_thumbnail_cache(limit, age);
            });
        }
        if let Some(interrupted) = config::interrupted_operation() {
            app.status = format!(
                "Previous Hoard session ended during a file operation — review {}",
                config::operation_journal_path().display()
            );
            app.operation_errors.push(interrupted);
            config::clear_operation_journal();
        }
        app.start_refresh();
        app
    }

    fn current_path(&self) -> &Path {
        &self.tabs[self.active_tab].path
    }
    fn rebuild_entry_index(&mut self) {
        self.entry_index = self
            .entries
            .iter()
            .enumerate()
            .map(|(idx, entry)| (entry.path.clone(), idx))
            .collect();
    }
    fn in_trash(&self) -> bool {
        self.current_path() == fs_model::trash_files_dir()
    }
    fn sync_address(&mut self) {
        self.address = self.current_path().to_string_lossy().to_string();
    }

    fn persist_session(&self) {
        if !self.settings.remember_tabs {
            return;
        }
        let state = config::SessionState {
            tabs: self
                .tabs
                .iter()
                .map(|t| t.path.to_string_lossy().to_string())
                .collect(),
            tab_state: self
                .tabs
                .iter()
                .map(|t| config::SessionTab {
                    path: t.path.to_string_lossy().to_string(),
                    back: t
                        .back
                        .iter()
                        .map(|p| p.to_string_lossy().to_string())
                        .collect(),
                    forward: t
                        .forward
                        .iter()
                        .map(|p| p.to_string_lossy().to_string())
                        .collect(),
                    search: t.search.clone(),
                    selected: t
                        .selected_paths
                        .iter()
                        .map(|p| p.to_string_lossy().to_string())
                        .collect(),
                    view: if t.view == ViewMode::Icons {
                        "icons".into()
                    } else {
                        "details".into()
                    },
                    scroll_y: t.scroll_y,
                })
                .collect(),
            active_tab: self.active_tab,
        };
        let _ = config::save_session(&state);
    }

    fn write_system_file_clipboard(&mut self, paths: &[PathBuf], mode: ClipboardMode) {
        if paths.is_empty() {
            return;
        }
        let action = if matches!(mode, ClipboardMode::Move) {
            "cut"
        } else {
            "copy"
        };
        let mut payload = String::from(action);
        for path in paths {
            if let Ok(url) = Url::from_file_path(path) {
                payload.push('\n');
                payload.push_str(url.as_str());
            }
        }
        let spawn = Command::new("wl-copy")
            .args(["--type", "x-special/gnome-copied-files"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        if let Ok(mut child) = spawn {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(payload.as_bytes());
            }
            let _ = child.wait();
        }
    }

    fn write_system_text_clipboard(&mut self, text: &str) {
        let spawn = Command::new("wl-copy")
            .args(["--type", "text/plain;charset=utf-8"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        match spawn {
            Ok(mut child) => {
                let write_ok = child
                    .stdin
                    .take()
                    .map(|mut stdin| stdin.write_all(text.as_bytes()).is_ok())
                    .unwrap_or(false);
                let wait_ok = child.wait().map(|status| status.success()).unwrap_or(false);
                self.status = if write_ok && wait_ok {
                    "Diagnostics copied to clipboard".into()
                } else {
                    "Could not copy diagnostics to clipboard".into()
                };
            }
            Err(e) => self.status = format!("Could not start wl-copy: {e}"),
        }
    }

    fn open_path_external(&mut self, path: &Path, label: &str) {
        let value = path.to_string_lossy().to_string();
        match Self::spawn_quiet("xdg-open", &[&value]) {
            Ok(_) => self.status = format!("Opened {label}"),
            Err(e) => self.status = format!("Could not open {label}: {e}"),
        }
    }

    fn read_system_file_clipboard(&self) -> Option<ClipboardItem> {
        let output = Command::new("wl-paste")
            .args(["--no-newline", "--type", "x-special/gnome-copied-files"])
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8(output.stdout).ok()?;
        let mut lines = text.lines();
        let mode = match lines.next()?.trim() {
            "cut" => ClipboardMode::Move,
            "copy" => ClipboardMode::Copy,
            _ => return None,
        };
        let paths: Vec<PathBuf> = lines
            .filter_map(|line| Url::parse(line.trim()).ok())
            .filter_map(|url| url.to_file_path().ok())
            .filter(|p| p.exists())
            .collect();
        if paths.is_empty() {
            None
        } else {
            Some(ClipboardItem { paths, mode })
        }
    }

    fn save_active_tab_state(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.search = self.search.clone();
            tab.selected = self.selected.clone();
            tab.selected_paths = self.selected_paths.iter().cloned().collect();
            tab.view = self.view;
        }
    }

    fn switch_to_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() || idx == self.active_tab {
            return;
        }
        self.save_active_tab_state();
        self.cancel_recursive_search();
        self.active_tab = idx;
        self.search = self.tabs[idx].search.clone();
        self.selected = self.tabs[idx].selected.clone();
        self.selected_paths = self.tabs[idx].selected_paths.iter().cloned().collect();
        if self.selected_paths.is_empty() {
            self.selected_paths.extend(self.selected.clone());
        }
        self.view = self.tabs[idx].view;
        self.entries.clear();
        self.entry_index.clear();
        self.directory_rx = None;
        self.metadata_rx = None;
        self.metadata_loading = false;
        self.sync_address();
        self.dirty = true;
        self.persist_session();
    }

    fn new_tab(&mut self, path: PathBuf, activate: bool) {
        self.save_active_tab_state();
        let idx = self.tabs.len();
        self.tabs.push(Tab::new(path, self.view));
        if activate {
            self.active_tab = idx;
            self.search.clear();
            self.selected = None;
            self.selected_paths.clear();
            self.entries.clear();
            self.entry_index.clear();
            self.sync_address();
            self.dirty = true;
        }
        self.persist_session();
    }

    fn close_tab(&mut self, idx: usize) {
        if self.tabs.len() <= 1 || idx >= self.tabs.len() {
            return;
        }
        self.save_active_tab_state();
        self.cancel_recursive_search();
        let closed = self.tabs.remove(idx);
        self.closed_tabs.push(closed);
        if self.closed_tabs.len() > 16 {
            self.closed_tabs.remove(0);
        }
        if idx < self.active_tab {
            self.active_tab -= 1;
        } else if self.active_tab >= self.tabs.len() {
            self.active_tab = self.tabs.len() - 1;
        }
        let tab = self.tabs[self.active_tab].clone();
        self.search = tab.search;
        self.selected = tab.selected;
        self.selected_paths = tab.selected_paths.iter().cloned().collect();
        if self.selected_paths.is_empty() {
            self.selected_paths.extend(self.selected.clone());
        }
        self.view = tab.view;
        self.entries.clear();
        self.entry_index.clear();
        self.sync_address();
        self.dirty = true;
        self.persist_session();
    }

    fn reopen_closed_tab(&mut self) {
        if let Some(tab) = self.closed_tabs.pop() {
            self.save_active_tab_state();
            self.cancel_recursive_search();
            self.tabs.push(tab);
            self.active_tab = self.tabs.len() - 1;
            let tab = self.tabs[self.active_tab].clone();
            self.search = tab.search;
            self.selected = tab.selected;
            self.selected_paths = tab.selected_paths.iter().cloned().collect();
            if self.selected_paths.is_empty() {
                self.selected_paths.extend(self.selected.clone());
            }
            self.view = tab.view;
            self.entries.clear();
            self.entry_index.clear();
            self.sync_address();
            self.dirty = true;
            self.persist_session();
        }
    }

    fn selected_items(&self) -> Vec<PathBuf> {
        if !self.selected_paths.is_empty() {
            let mut v: Vec<_> = self.selected_paths.iter().cloned().collect();
            v.sort();
            v
        } else {
            self.selected.clone().into_iter().collect()
        }
    }

    fn start_selected_folder_stats(&mut self) {
        if self.selected_paths.len() != 1 {
            if let Some(cancel) = self.folder_stats_cancel.take() {
                cancel.store(true, Ordering::Relaxed);
            }
            self.folder_stats_rx = None;
            self.folder_stats_path = None;
            self.folder_stats = None;
            return;
        }
        let selected = self.selected.clone();
        let is_dir = selected
            .as_ref()
            .and_then(|path| self.entry_index.get(path))
            .and_then(|idx| self.entries.get(*idx))
            .is_some_and(|entry| entry.is_dir);

        if !is_dir {
            if let Some(cancel) = self.folder_stats_cancel.take() {
                cancel.store(true, Ordering::Relaxed);
            }
            self.folder_stats_rx = None;
            self.folder_stats_path = None;
            self.folder_stats = None;
            return;
        }

        let Some(path) = selected else { return };
        if self.folder_stats_path.as_ref() == Some(&path)
            && (self.folder_stats_rx.is_some() || self.folder_stats.is_some())
        {
            return;
        }

        if let Some(cancel) = self.folder_stats_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.folder_stats = None;
        self.folder_stats_path = Some(path.clone());
        let (tx, rx) = mpsc::channel();
        self.folder_stats_rx = Some(rx);
        let cancel = Arc::new(AtomicBool::new(false));
        self.folder_stats_cancel = Some(cancel.clone());
        let repaint = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let stats = fs_model::folder_stats(&path, &cancel);
            if !cancel.load(Ordering::Relaxed) && tx.send((path, stats)).is_ok() {
                repaint.request_repaint();
            }
        });
    }

    fn poll_folder_stats(&mut self) {
        let result = match self.folder_stats_rx.as_ref().map(|rx| rx.try_recv()) {
            Some(Ok(result)) => Some(result),
            Some(Err(TryRecvError::Disconnected)) => {
                self.folder_stats_rx = None;
                None
            }
            _ => None,
        };
        let Some((path, stats)) = result else { return };
        self.folder_stats_rx = None;
        self.folder_stats_cancel = None;
        if self.selected.as_ref() == Some(&path) {
            self.folder_stats_path = Some(path);
            self.folder_stats = stats;
        }
    }

    fn select_entry(&mut self, path: PathBuf, additive: bool, range: bool) {
        let range_anchor = if range {
            self.selection_anchor
                .clone()
                .or_else(|| self.selected.clone())
        } else {
            None
        };
        if let Some(anchor) = range_anchor {
            let a = self.entries.iter().position(|e| e.path == anchor);
            let b = self.entries.iter().position(|e| e.path == path);
            if let (Some(a), Some(b)) = (a, b) {
                let (start, end) = if a <= b { (a, b) } else { (b, a) };
                if !additive {
                    self.selected_paths.clear();
                }
                for entry in &self.entries[start..=end] {
                    self.selected_paths.insert(entry.path.clone());
                }
                self.selected = Some(path.clone());
                self.selection_anchor = Some(anchor);
                self.save_active_tab_state();
                self.persist_session();
                self.start_selected_folder_stats();
                return;
            }
        }
        if additive {
            if !self.selected_paths.insert(path.clone()) {
                self.selected_paths.remove(&path);
            }
            self.selected = if self.selected_paths.contains(&path) {
                Some(path.clone())
            } else {
                self.selected_paths.iter().next().cloned()
            };
        } else {
            self.selected_paths.clear();
            self.selected_paths.insert(path.clone());
            self.selected = Some(path.clone());
        }
        self.selection_anchor = Some(path);
        self.save_active_tab_state();
        self.persist_session();
        self.start_selected_folder_stats();
    }

    fn select_all_visible(&mut self) {
        let paths: Vec<PathBuf> = match self.filtered_indices() {
            Some(indices) => indices
                .into_iter()
                .filter_map(|idx| self.entries.get(idx).map(|entry| entry.path.clone()))
                .collect(),
            None => self
                .entries
                .iter()
                .map(|entry| entry.path.clone())
                .collect(),
        };
        self.selected_paths = paths.iter().cloned().collect();
        self.selected = paths.first().cloned();
        self.selection_anchor = self.selected.clone();
        self.save_active_tab_state();
        self.persist_session();
        self.start_selected_folder_stats();
    }

    fn clear_selection(&mut self) {
        self.selected = None;
        self.selected_paths.clear();
        self.selection_anchor = None;
        self.save_active_tab_state();
        self.persist_session();
        self.start_selected_folder_stats();
    }

    fn invert_visible_selection(&mut self) {
        let paths: Vec<PathBuf> = match self.filtered_indices() {
            Some(indices) => indices
                .into_iter()
                .filter_map(|idx| self.entries.get(idx).map(|entry| entry.path.clone()))
                .collect(),
            None => self
                .entries
                .iter()
                .map(|entry| entry.path.clone())
                .collect(),
        };
        let inverted: HashSet<PathBuf> = paths
            .iter()
            .filter(|path| !self.selected_paths.contains(*path))
            .cloned()
            .collect();
        self.selected_paths = inverted;
        self.selected = paths
            .iter()
            .find(|path| self.selected_paths.contains(*path))
            .cloned();
        self.selection_anchor = self.selected.clone();
        self.save_active_tab_state();
        self.persist_session();
        self.start_selected_folder_stats();
    }

    fn visible_detail_columns(&self) -> Vec<DetailColumn> {
        let mut out = vec![DetailColumn::Name];
        for column in DetailColumn::ALL
            .into_iter()
            .filter(|column| *column != DetailColumn::Name)
        {
            if self
                .settings
                .details_columns
                .iter()
                .any(|key| key == column.key())
            {
                out.push(column);
            }
        }
        out
    }

    fn detail_column_width(&self, column: DetailColumn) -> f32 {
        self.settings
            .details_column_widths
            .get(column.key())
            .copied()
            .unwrap_or_else(|| column.default_width())
            .clamp(column.min_width(), 640.0)
    }

    fn set_detail_column_width(&mut self, column: DetailColumn, width: f32) {
        self.settings.details_column_widths.insert(
            column.key().to_string(),
            width.clamp(column.min_width(), 640.0),
        );
    }

    fn sort_by(&mut self, column: SortColumn, toggle: bool) {
        if toggle && self.sort == column {
            self.sort_ascending = !self.sort_ascending;
        } else {
            self.sort = column;
            self.sort_ascending = true;
        }
        if matches!(
            column,
            SortColumn::Size
                | SortColumn::Modified
                | SortColumn::Created
                | SortColumn::Owner
                | SortColumn::Permissions
        ) {
            self.start_metadata_load();
        }
        fs_model::sort_entries(&mut self.entries, self.sort, self.sort_ascending);
        self.rebuild_entry_index();
        self.persist_browsing_settings();
    }

    fn sort_context_menu(&mut self, ui: &mut egui::Ui) {
        for column in DetailColumn::ALL {
            let sort = column.sort_column();
            if ui
                .selectable_label(self.sort == sort, column.label())
                .clicked()
            {
                self.sort_by(sort, false);
                ui.close();
            }
        }
        ui.separator();
        if ui
            .selectable_label(self.sort_ascending, "Ascending")
            .clicked()
        {
            self.sort_ascending = true;
            fs_model::sort_entries(&mut self.entries, self.sort, self.sort_ascending);
            self.rebuild_entry_index();
            self.persist_browsing_settings();
            ui.close();
        }
        if ui
            .selectable_label(!self.sort_ascending, "Descending")
            .clicked()
        {
            self.sort_ascending = false;
            fs_model::sort_entries(&mut self.entries, self.sort, self.sort_ascending);
            self.rebuild_entry_index();
            self.persist_browsing_settings();
            ui.close();
        }
    }

    fn columns_context_menu(&mut self, ui: &mut egui::Ui) {
        ui.add_enabled(false, egui::Button::new("✓ Name"));
        for column in DetailColumn::ALL
            .into_iter()
            .filter(|column| *column != DetailColumn::Name)
        {
            let key = column.key();
            let mut visible = self.settings.details_columns.iter().any(|item| item == key);
            if ui.checkbox(&mut visible, column.label()).changed() {
                if visible {
                    if !self.settings.details_columns.iter().any(|item| item == key) {
                        self.settings.details_columns.push(key.to_string());
                    }
                    if matches!(
                        column,
                        DetailColumn::Size
                            | DetailColumn::Modified
                            | DetailColumn::Created
                            | DetailColumn::Owner
                            | DetailColumn::Permissions
                    ) {
                        self.start_metadata_load();
                    }
                } else {
                    self.settings.details_columns.retain(|item| item != key);
                }
                let _ = config::save_settings(&self.settings);
                self.egui_ctx.request_repaint();
            }
        }
        ui.separator();
        if ui.button("Reset column widths").clicked() {
            self.settings.details_column_widths.clear();
            let _ = config::save_settings(&self.settings);
            self.egui_ctx.request_repaint();
            ui.close();
        }
    }

    fn selection_context_menu(&mut self, ui: &mut egui::Ui) {
        if ui.button("Select All").clicked() {
            self.select_all_visible();
            ui.close();
        }
        if ui.button("Invert Selection").clicked() {
            self.invert_visible_selection();
            ui.close();
        }
        if ui.button("Clear Selection").clicked() {
            self.clear_selection();
            ui.close();
        }
    }

    fn apply_rubber_band_selection(
        &mut self,
        selection_rect: Rect,
        filtered: &Option<Vec<usize>>,
        layout: RubberBandGridLayout,
    ) {
        let Some(state) = self.rubber_band.as_ref() else {
            return;
        };
        let mut selected = if state.additive {
            state.base.clone()
        } else {
            HashSet::new()
        };
        let row_stride = layout.card_size.y + layout.row_gap;
        let top = (selection_rect.top() - layout.content_rect.top() - layout.padding.y).max(0.0);
        let bottom =
            (selection_rect.bottom() - layout.content_rect.top() - layout.padding.y).max(0.0);
        let first_row = (top / row_stride).floor().max(0.0) as usize;
        let last_row = ((bottom / row_stride).floor().max(0.0) as usize)
            .min(layout.item_count.saturating_sub(1) / layout.columns);
        for row in first_row..=last_row {
            for col in 0..layout.columns {
                let pos = row * layout.columns + col;
                if pos >= layout.item_count {
                    break;
                }
                let entry_idx = filtered
                    .as_ref()
                    .and_then(|indices| indices.get(pos).copied())
                    .unwrap_or(pos);
                let Some(entry) = self.entries.get(entry_idx) else {
                    continue;
                };
                let card_left = layout.content_rect.left()
                    + layout.padding.x
                    + col as f32 * (layout.card_size.x + layout.col_gap);
                let card_top =
                    layout.content_rect.top() + layout.padding.y + row as f32 * row_stride;
                let card_rect =
                    Rect::from_min_size(Pos2::new(card_left, card_top), layout.card_size);
                if card_rect.intersects(selection_rect) {
                    selected.insert(entry.path.clone());
                }
            }
        }
        self.selected_paths = selected;
        self.selected = self
            .entries
            .iter()
            .find(|entry| self.selected_paths.contains(&entry.path))
            .map(|entry| entry.path.clone());
    }

    fn move_keyboard_selection(&mut self, delta: isize, extend: bool) {
        if self.entries.is_empty() {
            return;
        }
        let current = self
            .selected
            .as_ref()
            .and_then(|path| self.entries.iter().position(|e| &e.path == path))
            .unwrap_or(0);
        let next = (current as isize + delta)
            .clamp(0, self.entries.len().saturating_sub(1) as isize) as usize;
        let path = self.entries[next].path.clone();
        self.select_entry(path, false, extend);
    }

    fn select_keyboard_index(&mut self, index: usize, extend: bool) {
        if let Some(entry) = self.entries.get(index) {
            self.select_entry(entry.path.clone(), false, extend);
        }
    }

    fn navigate(&mut self, path: PathBuf) {
        let tab = &mut self.tabs[self.active_tab];
        if tab.path != path {
            tab.back.push(tab.path.clone());
            tab.forward.clear();
            tab.path = path;
        }
        self.selected = None;
        self.selected_paths.clear();
        self.selection_anchor = None;
        self.search.clear();
        self.cancel_recursive_search();
        self.entries.clear();
        self.entry_index.clear();
        self.directory_rx = None;
        self.metadata_rx = None;
        self.metadata_loading = false;
        self.sync_address();
        self.dirty = true;
        self.persist_session();
    }

    fn go_back(&mut self) {
        let tab = &mut self.tabs[self.active_tab];
        if let Some(p) = tab.back.pop() {
            tab.forward.push(tab.path.clone());
            tab.path = p;
            self.selected = None;
            self.selected_paths.clear();
            self.selection_anchor = None;
            self.cancel_recursive_search();
            self.entries.clear();
            self.entry_index.clear();
            self.directory_rx = None;
            self.metadata_rx = None;
            self.metadata_loading = false;
            self.sync_address();
            self.dirty = true;
            self.persist_session();
        }
    }

    fn go_forward(&mut self) {
        let tab = &mut self.tabs[self.active_tab];
        if let Some(p) = tab.forward.pop() {
            tab.back.push(tab.path.clone());
            tab.path = p;
            self.selected = None;
            self.selected_paths.clear();
            self.selection_anchor = None;
            self.cancel_recursive_search();
            self.entries.clear();
            self.entry_index.clear();
            self.directory_rx = None;
            self.metadata_rx = None;
            self.metadata_loading = false;
            self.sync_address();
            self.dirty = true;
            self.persist_session();
        }
    }

    fn go_up(&mut self) {
        if self.in_trash() {
            if let Some(home) = dirs::home_dir() {
                self.navigate(home);
            }
        } else if let Some(p) = self.current_path().parent() {
            self.navigate(p.to_path_buf());
        }
    }

    fn cancel_recursive_search(&mut self) {
        if let Some(cancel) = self.recursive_search_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.recursive_search_rx = None;
        self.recursive_searching = false;
    }

    fn start_refresh(&mut self) {
        self.cancel_recursive_search();
        let path = self.current_path().to_path_buf();
        self.last_directory_signature = cache::directory_signature(&path);
        self.next_directory_watch = Instant::now() + Duration::from_secs(2);
        let show_hidden = self.show_hidden;
        let sort = self.sort;
        let sort_ascending = self.sort_ascending;

        // Revisit cache: show the last known listing immediately, then always
        // refresh in the background so external changes are picked up.
        if self.settings.directory_cache {
            if let Some(mut cached) = cache::load_directory(&path, show_hidden, false) {
                fs_model::sort_entries(&mut cached, sort, sort_ascending);
                self.entries = cached;
                self.rebuild_entry_index();
                self.cache_hits = self.cache_hits.saturating_add(1);
                self.status = format!("{} items — cached, refreshing…", self.entries.len());
            } else {
                self.cache_misses = self.cache_misses.saturating_add(1);
            }
        }

        let (tx, rx) = mpsc::channel();
        self.directory_rx = Some(rx);
        self.loading = true;
        self.dirty = false;
        if self.entries.is_empty() {
            self.status = format!(
                "Loading {}…",
                path.file_name().and_then(|s| s.to_str()).unwrap_or("/")
            );
        }

        let space_path = path.clone();
        let (space_tx, space_rx) = mpsc::channel();
        self.space_rx = Some(space_rx);
        self.current_space = None;
        let space_repaint = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let space = fs_model::disk_space(&space_path);
            if space_tx.send((space_path, space)).is_ok() {
                space_repaint.request_repaint();
            }
        });

        let directory_repaint = self.egui_ctx.clone();
        let use_cache = self.settings.directory_cache;
        std::thread::spawn(move || {
            let (mut entries, error) = match fs_model::read_directory(&path, show_hidden) {
                Ok(entries) => (entries, None),
                Err(err) => (Vec::new(), Some(err.to_string())),
            };
            fs_model::sort_entries(&mut entries, sort, sort_ascending);
            if error.is_none() && use_cache {
                let _ = cache::store_directory(&path, show_hidden, &entries);
            }
            let mounts = fs_model::discover_mounts();
            let unmounted_devices = fs_model::discover_unmounted_devices();
            if tx
                .send(DirectoryLoadResult {
                    path,
                    entries,
                    mounts,
                    unmounted_devices,
                    error,
                })
                .is_ok()
            {
                directory_repaint.request_repaint();
            }
        });
    }

    fn recursive_filter(&self) -> SearchFilter {
        SearchFilter {
            query: self.search.trim().to_string(),
            type_filter: self.search_type_filter.trim().to_string(),
            min_size: self
                .search_min_size_mb
                .trim()
                .parse::<u64>()
                .ok()
                .map(|m| m.saturating_mul(1024 * 1024)),
            max_age_days: self.search_max_age_days.trim().parse::<u64>().ok(),
        }
    }

    fn start_recursive_search(&mut self) {
        self.cancel_recursive_search();
        let filter = self.recursive_filter();
        if filter.query.is_empty() {
            self.dirty = true;
            return;
        }
        let root = self.current_path().to_path_buf();
        if fs_model::is_remote_path(&root) && !self.settings.recursive_search_remote {
            self.status =
                "Recursive search on remote filesystems is disabled in Preferences".into();
            self.recursive_searching = false;
            return;
        }
        let key = format!(
            "{}|{}|{:?}|{:?}",
            filter.query, filter.type_filter, filter.min_size, filter.max_age_days
        );
        self.recursive_search_key = key.clone();
        self.recursive_searching = true;
        self.loading = false;
        self.entries.clear();
        self.rebuild_entry_index();
        self.status = format!("Searching {} recursively…", root.display());
        let max_results = self.settings.recursive_search_max_results.max(50);
        let show_hidden = self.show_hidden;
        let cancel = Arc::new(AtomicBool::new(false));
        self.recursive_search_cancel = Some(cancel.clone());
        let (tx, rx) = mpsc::channel();
        self.recursive_search_rx = Some(rx);
        let repaint = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let entries =
                search::recursive_search(&root, &filter, show_hidden, max_results, &cancel);
            if !cancel.load(Ordering::Relaxed) {
                let _ = tx.send(RecursiveSearchMessage::Done { root, key, entries });
                repaint.request_repaint();
            }
        });
    }

    fn poll_recursive_search(&mut self) {
        let msg = match self.recursive_search_rx.as_ref().map(|rx| rx.try_recv()) {
            Some(Ok(m)) => Some(m),
            Some(Err(TryRecvError::Disconnected)) => {
                self.recursive_search_rx = None;
                self.recursive_searching = false;
                None
            }
            _ => None,
        };
        let Some(RecursiveSearchMessage::Done {
            root,
            key,
            mut entries,
        }) = msg
        else {
            return;
        };
        self.recursive_search_rx = None;
        self.recursive_search_cancel = None;
        self.recursive_searching = false;
        if self.recursive_search && self.current_path() == root && self.recursive_search_key == key
        {
            fs_model::sort_entries(&mut entries, self.sort, self.sort_ascending);
            self.entries = entries;
            self.rebuild_entry_index();
            self.status = format!("{} recursive result(s)", self.entries.len());
        }
    }

    fn poll_space(&mut self) {
        let result = match self.space_rx.as_ref().map(|rx| rx.try_recv()) {
            Some(Ok(result)) => Some(result),
            Some(Err(TryRecvError::Disconnected)) => {
                self.space_rx = None;
                None
            }
            _ => None,
        };
        let Some((path, space)) = result else {
            return;
        };
        self.space_rx = None;
        if path == self.current_path() {
            self.current_space = space;
        }
    }

    fn watch_current_directory(&mut self) {
        if Instant::now() < self.next_directory_watch
            || self.file_op_busy
            || self.recursive_searching
        {
            return;
        }
        self.next_directory_watch = Instant::now() + Duration::from_secs(2);
        let current = cache::directory_signature(self.current_path());
        if self.last_directory_signature.is_some() && current != self.last_directory_signature {
            self.last_directory_signature = current;
            cache::invalidate_directory(self.current_path());
            self.dirty = true;
            self.status = "Folder changed externally — refreshing…".into();
        }
    }

    fn poll_directory(&mut self) {
        let result = match self.directory_rx.as_ref().map(|rx| rx.try_recv()) {
            Some(Ok(result)) => Some(result),
            Some(Err(TryRecvError::Disconnected)) => {
                self.directory_rx = None;
                self.loading = false;
                self.status = "Could not load directory".into();
                None
            }
            _ => None,
        };

        let Some(result) = result else {
            return;
        };
        self.directory_rx = None;
        self.loading = false;

        if result.path != self.current_path() {
            return;
        }

        self.entries = result.entries;
        self.rebuild_entry_index();
        self.mounts = result.mounts;
        self.unmounted_devices = result.unmounted_devices;
        if let Some(error) = result.error {
            self.status = format!("Could not load {}: {error}", self.current_path().display());
        }
        let live_paths: HashSet<PathBuf> = self.entries.iter().map(|e| e.path.clone()).collect();
        self.selected_paths.retain(|path| live_paths.contains(path));
        if self
            .selected
            .as_ref()
            .map(|path| !live_paths.contains(path))
            .unwrap_or(false)
        {
            self.selected = None;
        }
        self.last_directory_signature = cache::directory_signature(self.current_path());
        self.thumbnails.retain(|key, _| {
            live_paths.contains(&key.path) || self.selected.as_ref() == Some(&key.path)
        });
        self.thumbnail_pending
            .retain(|key| live_paths.contains(&key.path));
        self.thumbnail_failed
            .retain(|key| live_paths.contains(&key.path));
        if self.status.starts_with("Loading ") {
            self.status = format!("{} items", self.entries.len());
        }
        if self.view == ViewMode::Details {
            self.start_metadata_load();
        }
    }

    fn start_metadata_load(&mut self) {
        if self.metadata_loading
            || self.entries.is_empty()
            || (self.view != ViewMode::Details
                && !matches!(
                    self.sort,
                    SortColumn::Size
                        | SortColumn::Modified
                        | SortColumn::Created
                        | SortColumn::Owner
                        | SortColumn::Permissions
                ))
        {
            return;
        }
        let path = self.current_path().to_path_buf();
        let paths: Vec<PathBuf> = self
            .entries
            .iter()
            .filter(|e| !e.metadata_loaded)
            .map(|e| e.path.clone())
            .collect();
        if paths.is_empty() {
            return;
        }

        let (tx, rx) = mpsc::channel();
        self.metadata_rx = Some(rx);
        self.metadata_loading = true;
        let repaint = self.egui_ctx.clone();
        std::thread::spawn(move || {
            for chunk in paths.chunks(96) {
                let batch = fs_model::load_metadata(chunk);
                if tx
                    .send(MetadataMessage::Batch(path.clone(), batch))
                    .is_err()
                {
                    return;
                }
                repaint.request_repaint();
            }
            if tx.send(MetadataMessage::Done(path)).is_ok() {
                repaint.request_repaint();
            }
        });
    }

    fn poll_metadata(&mut self) {
        // Keep the UI thread responsive even when thousands of metadata jobs
        // complete at once. Results are applied through a path->row index and
        // only a bounded number of channel messages are handled per frame.
        const MAX_MESSAGES_PER_FRAME: usize = 3;
        let deadline = Instant::now() + Duration::from_millis(4);
        let mut processed = 0usize;
        for _ in 0..MAX_MESSAGES_PER_FRAME {
            if Instant::now() >= deadline {
                break;
            }
            let message = match self.metadata_rx.as_ref().map(|rx| rx.try_recv()) {
                Some(Ok(msg)) => Some(msg),
                Some(Err(TryRecvError::Disconnected)) => {
                    self.metadata_rx = None;
                    self.metadata_loading = false;
                    None
                }
                _ => None,
            };
            let Some(message) = message else {
                break;
            };
            processed += 1;
            match message {
                MetadataMessage::Batch(path, batch) => {
                    if path != self.current_path() {
                        continue;
                    }
                    for meta in batch {
                        let idx = self.entry_index.get(&meta.path).copied();
                        if let Some(entry) = idx.and_then(|idx| self.entries.get_mut(idx)) {
                            entry.size = meta.size;
                            entry.modified = meta.modified;
                            entry.created = meta.created;
                            entry.accessed = meta.accessed;
                            entry.owner = meta.owner;
                            entry.mode = meta.mode;
                            entry.metadata_loaded = true;
                        }
                    }
                }
                MetadataMessage::Done(path) => {
                    if path == self.current_path() {
                        if matches!(
                            self.sort,
                            SortColumn::Size
                                | SortColumn::Modified
                                | SortColumn::Created
                                | SortColumn::Owner
                                | SortColumn::Permissions
                        ) {
                            fs_model::sort_entries(
                                &mut self.entries,
                                self.sort,
                                self.sort_ascending,
                            );
                            self.rebuild_entry_index();
                        }
                        if self.settings.directory_cache {
                            let _ = cache::store_directory(&path, self.show_hidden, &self.entries);
                        }
                    }
                    self.metadata_rx = None;
                    self.metadata_loading = false;
                    break;
                }
            }
        }
        if processed > 0 && self.metadata_rx.is_some() {
            self.egui_ctx.request_repaint();
        }
    }

    fn persist_browsing_settings(&mut self) {
        self.settings.show_hidden = self.show_hidden;
        self.settings.default_view = if self.view == ViewMode::Icons {
            "icons".into()
        } else {
            "details".into()
        };
        self.settings.sort_column = match self.sort {
            SortColumn::Name => "name",
            SortColumn::Type => "type",
            SortColumn::Size => "size",
            SortColumn::Modified => "modified",
            SortColumn::Created => "created",
            SortColumn::Owner => "owner",
            SortColumn::Permissions => "permissions",
        }
        .into();
        self.settings.sort_ascending = self.sort_ascending;
        if let Err(e) = config::save_settings(&self.settings) {
            self.status = format!("Could not save settings: {e}");
        }
    }

    fn start_file_operation(&mut self, paths: Vec<PathBuf>, dest: PathBuf, mode: ClipboardMode) {
        if paths.is_empty() || !dest.is_dir() {
            return;
        }
        let conflicts = paths
            .iter()
            .filter(|p| {
                if matches!(mode, ClipboardMode::Move) && p.parent() == Some(dest.as_path()) {
                    return false;
                }
                fs_model::direct_destination(p, &dest)
                    .map(|d| d.exists())
                    .unwrap_or(false)
            })
            .count();
        if conflicts > 0 {
            self.pending_collision = Some(PendingCollision {
                paths,
                dest,
                mode,
                conflicts,
            });
            self.status = format!("{conflicts} name conflict(s) need a decision");
            return;
        }
        self.enqueue_file_operation(QueuedFileOperation {
            paths,
            dest,
            mode,
            collision: fs_model::CollisionPolicy::KeepBoth,
        });
    }

    fn enqueue_operation(&mut self, op: QueuedOperation) {
        if self.file_op_busy {
            self.file_op_queue.push_back(op);
            self.status = format!("Operation queued — {} waiting", self.file_op_queue.len());
        } else {
            self.launch_operation(op);
        }
    }

    fn enqueue_file_operation(&mut self, op: QueuedFileOperation) {
        self.enqueue_operation(QueuedOperation::Transfer(op));
    }

    fn launch_operation(&mut self, operation: QueuedOperation) {
        let journal_label = match &operation {
            QueuedOperation::Transfer(op) => format!(
                "transfer {} item(s) to {}",
                op.paths.len(),
                op.dest.display()
            ),
            QueuedOperation::Trash(paths) => format!("trash {} item(s)", paths.len()),
            QueuedOperation::Restore(paths) => format!("restore {} item(s)", paths.len()),
            QueuedOperation::Rename { src, .. } => format!("rename {}", src.display()),
            QueuedOperation::CreateFolder { parent, .. } => {
                format!("create folder in {}", parent.display())
            }
            QueuedOperation::CreateFile { parent, .. } => {
                format!("create file in {}", parent.display())
            }
            QueuedOperation::Duplicate(path) => format!("duplicate {}", path.display()),
            QueuedOperation::DeletePermanent(paths) => {
                format!("permanently delete {} item(s)", paths.len())
            }
            QueuedOperation::EmptyTrash => "empty trash".into(),
        };
        config::write_operation_journal(&journal_label);
        let (tx, rx) = mpsc::channel();
        self.file_op_rx = Some(rx);
        self.file_op_busy = true;
        self.file_op_progress = None;
        let cancel = Arc::new(AtomicBool::new(false));
        self.file_op_cancel = Some(cancel.clone());
        let repaint = self.egui_ctx.clone();

        match operation {
            QueuedOperation::Transfer(op) => {
                if op.paths.is_empty() {
                    self.file_op_busy = false;
                    self.file_op_rx = None;
                    return;
                }
                let total = op.paths.len();
                self.file_op_progress = Some((0, total, String::new(), 0, None));
                self.status = match op.mode {
                    ClipboardMode::Copy => format!("Copying {total} item(s)…"),
                    ClipboardMode::Move => format!("Moving {total} item(s)…"),
                };
                std::thread::spawn(move || {
                    let mut succeeded = 0usize;
                    let mut failed = 0usize;
                    let mut created = Vec::new();
                    let mut moved_pairs = Vec::new();
                    let mut errors = Vec::new();
                    let total_bytes = op.paths.iter().try_fold(0u64, |sum, p| {
                        let meta = std::fs::metadata(p).ok()?;
                        if meta.is_dir() {
                            None
                        } else {
                            Some(sum.saturating_add(meta.len()))
                        }
                    });
                    let mut bytes_done = 0u64;
                    for (idx, path) in op.paths.iter().enumerate() {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        let name = path
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("item")
                            .to_string();
                        let result = match op.mode {
                            ClipboardMode::Copy => fs_model::copy_item_with_policy_cancel(
                                path,
                                &op.dest,
                                op.collision,
                                Some(&cancel),
                            ),
                            ClipboardMode::Move => fs_model::move_item_with_policy_cancel(
                                path,
                                &op.dest,
                                op.collision,
                                Some(&cancel),
                            ),
                        };
                        match result {
                            Ok(Some(dest)) => {
                                succeeded += 1;
                                if matches!(op.mode, ClipboardMode::Move) {
                                    moved_pairs.push((path.clone(), dest));
                                } else if let Some(sig) = Signature::capture(&dest) {
                                    created.push((dest, sig));
                                }
                            }
                            Ok(None) => {}
                            Err(e) => {
                                failed += 1;
                                errors.push(format!("{}: {e}", path.display()));
                            }
                        }
                        if let Some(len) = std::fs::metadata(path)
                            .ok()
                            .filter(|meta| meta.is_file())
                            .map(|meta| meta.len())
                        {
                            bytes_done = bytes_done.saturating_add(len);
                        }
                        if tx
                            .send(FileOpMessage::Progress {
                                done: idx + 1,
                                total,
                                name,
                                bytes_done,
                                total_bytes,
                            })
                            .is_err()
                        {
                            return;
                        }
                        repaint.request_repaint();
                    }
                    let cancelled = cancel.load(Ordering::Relaxed);
                    let _ = tx.send(FileOpMessage::Done {
                        succeeded,
                        failed,
                        moved: matches!(op.mode, ClipboardMode::Move),
                        created,
                        moved_pairs,
                        errors,
                        cancelled,
                        undoable: op.collision != fs_model::CollisionPolicy::Replace,
                    });
                    repaint.request_repaint();
                });
            }
            simple => {
                self.status = match &simple {
                    QueuedOperation::Trash(paths) => {
                        format!("Moving {} item(s) to Trash…", paths.len())
                    }
                    QueuedOperation::Restore(paths) => {
                        format!("Restoring {} item(s)…", paths.len())
                    }
                    QueuedOperation::Rename { .. } => "Renaming…".into(),
                    QueuedOperation::CreateFolder { .. } => "Creating folder…".into(),
                    QueuedOperation::CreateFile { .. } => "Creating file…".into(),
                    QueuedOperation::Duplicate(_) => "Duplicating…".into(),
                    QueuedOperation::DeletePermanent(paths) => {
                        format!("Permanently deleting {} item(s)…", paths.len())
                    }
                    QueuedOperation::EmptyTrash => "Emptying Trash…".into(),
                    QueuedOperation::Transfer(_) => unreachable!(),
                };
                std::thread::spawn(move || {
                    let mut errors = Vec::new();
                    let mut undo = None;
                    let mut select = None;
                    let mut destructive = false;
                    let status = match simple {
                        QueuedOperation::Trash(paths) => {
                            let mut trashed = Vec::new();
                            for path in paths {
                                if cancel.load(Ordering::Relaxed) {
                                    break;
                                }
                                match fs_model::trash_item(&path) {
                                    Ok(trash) => trashed.push((path, trash)),
                                    Err(e) => errors.push(format!("{}: {e}", path.display())),
                                }
                            }
                            if !trashed.is_empty() {
                                undo = Some(UndoAction::Trash {
                                    pairs: trashed.clone(),
                                });
                            }
                            format!("Moved {} item(s) to Trash", trashed.len())
                        }
                        QueuedOperation::Restore(paths) => {
                            let mut restored = Vec::new();
                            for path in paths {
                                if cancel.load(Ordering::Relaxed) {
                                    break;
                                }
                                match fs_model::restore_trashed(&path) {
                                    Ok(dest) => {
                                        select = Some(dest.clone());
                                        if let Some(sig) = Signature::capture(&dest) {
                                            restored.push((path, dest, sig));
                                        }
                                    }
                                    Err(e) => errors.push(format!("{}: {e}", path.display())),
                                }
                            }
                            if !restored.is_empty() {
                                undo = Some(UndoAction::Restore {
                                    pairs: restored.clone(),
                                });
                            }
                            format!("Restored {} item(s)", restored.len())
                        }
                        QueuedOperation::Rename { src, new_name } => {
                            match fs_model::rename_item(&src, &new_name) {
                                Ok(dest) => {
                                    select = Some(dest.clone());
                                    undo = Some(UndoAction::Rename {
                                        from: src,
                                        to: dest.clone(),
                                    });
                                    format!(
                                        "Renamed to {}",
                                        dest.file_name().and_then(|s| s.to_str()).unwrap_or("item")
                                    )
                                }
                                Err(e) => {
                                    errors.push(e.to_string());
                                    "Rename failed".into()
                                }
                            }
                        }
                        QueuedOperation::CreateFolder { parent, name } => {
                            match fs_model::create_folder(&parent, &name) {
                                Ok(path) => {
                                    select = Some(path.clone());
                                    if let Some(sig) = Signature::capture(&path) {
                                        undo = Some(UndoAction::Created {
                                            label: "New Folder".into(),
                                            paths: vec![(path.clone(), sig)],
                                        });
                                    }
                                    format!("Created {}", path.display())
                                }
                                Err(e) => {
                                    errors.push(e.to_string());
                                    "Create folder failed".into()
                                }
                            }
                        }
                        QueuedOperation::CreateFile { parent, name } => {
                            match fs_model::create_text_file(&parent, &name) {
                                Ok(path) => {
                                    select = Some(path.clone());
                                    if let Some(sig) = Signature::capture(&path) {
                                        undo = Some(UndoAction::Created {
                                            label: "New File".into(),
                                            paths: vec![(path.clone(), sig)],
                                        });
                                    }
                                    format!("Created {}", path.display())
                                }
                                Err(e) => {
                                    errors.push(e.to_string());
                                    "Create file failed".into()
                                }
                            }
                        }
                        QueuedOperation::Duplicate(src) => match fs_model::duplicate_item(&src) {
                            Ok(path) => {
                                select = Some(path.clone());
                                if let Some(sig) = Signature::capture(&path) {
                                    undo = Some(UndoAction::Created {
                                        label: "Duplicate".into(),
                                        paths: vec![(path.clone(), sig)],
                                    });
                                }
                                format!(
                                    "Duplicated {}",
                                    src.file_name().and_then(|s| s.to_str()).unwrap_or("item")
                                )
                            }
                            Err(e) => {
                                errors.push(e.to_string());
                                "Duplicate failed".into()
                            }
                        },
                        QueuedOperation::DeletePermanent(paths) => {
                            destructive = true;
                            let mut deleted = 0usize;
                            for path in paths {
                                if cancel.load(Ordering::Relaxed) {
                                    break;
                                }
                                match fs_model::delete_permanently(&path) {
                                    Ok(()) => deleted += 1,
                                    Err(e) => errors.push(format!("{}: {e}", path.display())),
                                }
                            }
                            format!("Permanently deleted {deleted} item(s)")
                        }
                        QueuedOperation::EmptyTrash => {
                            destructive = true;
                            match fs_model::empty_trash() {
                                Ok(()) => "Trash emptied".into(),
                                Err(e) => {
                                    errors.push(e.to_string());
                                    "Could not empty Trash".into()
                                }
                            }
                        }
                        QueuedOperation::Transfer(_) => unreachable!(),
                    };
                    let _ = tx.send(FileOpMessage::SimpleDone {
                        status,
                        undo,
                        errors,
                        select,
                        destructive,
                    });
                    repaint.request_repaint();
                });
            }
        }
    }

    fn cancel_file_operation(&mut self) {
        if let Some(cancel) = &self.file_op_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        let cleared = self.file_op_queue.len();
        self.file_op_queue.clear();
        if self.file_op_busy {
            self.status = if cleared > 0 {
                format!("Cancelling after current item; cleared {cleared} queued operation(s)…")
            } else {
                "Cancelling after current item…".into()
            };
        }
    }

    fn poll_file_operations(&mut self) {
        const MAX_MESSAGES_PER_FRAME: usize = 8;
        let deadline = Instant::now() + Duration::from_millis(3);
        let mut finished = false;
        for _ in 0..MAX_MESSAGES_PER_FRAME {
            if Instant::now() >= deadline {
                break;
            }
            let msg = match self.file_op_rx.as_ref().map(|rx| rx.try_recv()) {
                Some(Ok(msg)) => Some(msg),
                Some(Err(TryRecvError::Disconnected)) => {
                    self.file_op_rx = None;
                    self.file_op_busy = false;
                    self.file_op_cancel = None;
                    None
                }
                _ => None,
            };
            let Some(msg) = msg else {
                break;
            };
            match msg {
                FileOpMessage::Progress {
                    done,
                    total,
                    name,
                    bytes_done,
                    total_bytes,
                } => {
                    self.file_op_progress =
                        Some((done, total, name.clone(), bytes_done, total_bytes));
                    self.status = if let Some(total_bytes) = total_bytes {
                        format!(
                            "{done}/{total}: {name} — {} / {}",
                            fs_model::format_size(bytes_done),
                            fs_model::format_size(total_bytes)
                        )
                    } else {
                        format!("{done}/{total}: {name}")
                    };
                }
                FileOpMessage::Done {
                    succeeded,
                    failed,
                    moved,
                    created,
                    moved_pairs,
                    errors,
                    cancelled,
                    undoable,
                } => {
                    config::clear_operation_journal();
                    self.file_op_rx = None;
                    self.file_op_busy = false;
                    self.file_op_cancel = None;
                    self.file_op_progress = None;
                    if moved && succeeded > 0 {
                        self.clipboard = None;
                        let _ = Command::new("wl-copy")
                            .arg("--clear")
                            .stdin(Stdio::null())
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .spawn();
                        if undoable && !moved_pairs.is_empty() {
                            self.undo_history
                                .push(UndoAction::Move { pairs: moved_pairs });
                        }
                    } else if undoable && !created.is_empty() {
                        self.undo_history.push(UndoAction::Created {
                            label: "Copy".into(),
                            paths: created,
                        });
                    }
                    if !errors.is_empty() {
                        self.operation_errors.extend(errors);
                        if self.operation_errors.len() > 32 {
                            self.operation_errors
                                .drain(..self.operation_errors.len() - 32);
                        }
                    }
                    self.status = if cancelled {
                        format!("Cancelled — {succeeded} completed, {failed} failed")
                    } else if !undoable && succeeded > 0 {
                        format!(
                            "Completed {succeeded} item(s) with Replace — this operation is not undoable"
                        )
                    } else if failed == 0 {
                        format!("Completed {succeeded} item(s)")
                    } else {
                        format!(
                            "Completed {succeeded} item(s); {failed} failed — see operation errors"
                        )
                    };
                    cache::invalidate_directory(self.current_path());
                    self.dirty = true;
                    finished = true;
                    break;
                }
                FileOpMessage::SimpleDone {
                    status,
                    undo,
                    errors,
                    select,
                    destructive,
                } => {
                    config::clear_operation_journal();
                    self.file_op_rx = None;
                    self.file_op_busy = false;
                    self.file_op_cancel = None;
                    self.file_op_progress = None;
                    if let Some(action) = undo {
                        self.undo_history.push(action);
                    }
                    if let Some(path) = select {
                        self.selected = Some(path.clone());
                        self.selected_paths.clear();
                        self.selected_paths.insert(path);
                    }
                    let had_errors = !errors.is_empty();
                    if had_errors {
                        self.operation_errors.extend(errors);
                        if self.operation_errors.len() > 32 {
                            self.operation_errors
                                .drain(..self.operation_errors.len() - 32);
                        }
                    }
                    self.status = if destructive {
                        format!("{status} — cannot be undone")
                    } else if !had_errors {
                        format!("{status} — Ctrl+Z to undo")
                    } else {
                        format!("{status}; some items failed — see operation errors")
                    };
                    cache::invalidate_directory(self.current_path());
                    self.dirty = true;
                    finished = true;
                    break;
                }
            }
        }
        if finished {
            if let Some(next) = self.file_op_queue.pop_front() {
                self.launch_operation(next);
            }
        } else if self.file_op_rx.is_some() {
            self.egui_ctx.request_repaint();
        }
    }

    fn undo_last(&mut self) {
        if self.file_op_busy {
            self.status = "Finish or cancel the active operation before Undo".into();
            return;
        }
        match self.undo_history.undo() {
            Ok(msg) => {
                self.status = msg;
                self.selected = None;
                self.selected_paths.clear();
                self.dirty = true;
            }
            Err(e) => self.status = format!("Undo refused: {e}"),
        }
    }

    fn redo_last(&mut self) {
        if self.file_op_busy {
            self.status = "Finish or cancel the active operation before Redo".into();
            return;
        }
        match self.undo_history.redo() {
            Ok(msg) => {
                self.status = msg;
                self.selected = None;
                self.selected_paths.clear();
                self.dirty = true;
            }
            Err(e) => self.status = format!("Redo refused: {e}"),
        }
    }

    fn drop_drag_on(&mut self, dest: &Path, copy: bool) {
        let Some(payload) = self.drag_payload.take() else {
            return;
        };
        let mode = if copy {
            ClipboardMode::Copy
        } else {
            ClipboardMode::Move
        };
        self.start_file_operation(payload.paths, dest.to_path_buf(), mode);
    }

    pub fn accept_external_drop_paths(&mut self, paths: Vec<PathBuf>, move_requested: bool) {
        if self.file_op_busy || paths.is_empty() {
            return;
        }
        let dest = self.current_path().to_path_buf();
        let mode = if move_requested {
            ClipboardMode::Move
        } else {
            ClipboardMode::Copy
        };
        self.start_file_operation(paths, dest, mode);
    }

    fn filtered_indices(&self) -> Option<Vec<usize>> {
        let q = self.search.trim().to_ascii_lowercase();
        if q.is_empty() {
            return None;
        }
        Some(
            self.entries
                .iter()
                .enumerate()
                .filter(|(_, e)| e.name.to_ascii_lowercase().contains(&q))
                .map(|(idx, _)| idx)
                .collect(),
        )
    }

    fn spawn_quiet(program: &str, args: &[&str]) -> std::io::Result<std::process::Child> {
        Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
    }

    fn open_entry(&mut self, entry: &FileEntry) {
        if entry.is_dir {
            if self.settings.open_folders_in_new_tab {
                self.new_tab(entry.path.clone(), true);
            } else {
                self.navigate(entry.path.clone());
            }
        } else {
            self.open_with_default(&entry.path, &entry.name);
        }
    }

    fn open_with_default(&mut self, path: &Path, label: &str) {
        let path_text = path.to_string_lossy().to_string();
        match Self::spawn_quiet("xdg-open", &[&path_text]) {
            Ok(_) => self.status = format!("Opened {label}"),
            Err(e) => self.status = format!("Could not open {label}: {e}"),
        }
    }

    fn open_with_chooser(&mut self, path: &Path, label: &str) {
        if self.open_with_rx.is_some() {
            self.status = "Application chooser is already opening…".into();
            return;
        }

        let path = path.to_path_buf();
        let label = label.to_string();
        let repaint = self.egui_ctx.clone();
        let (tx, rx) = mpsc::channel();
        self.open_with_rx = Some(rx);
        self.status = format!("Choose an application for {label}…");

        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<()> {
                let file = std::fs::File::open(&path)?;
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()?;
                runtime.block_on(async {
                    ashpd::desktop::open_uri::OpenFileRequest::default()
                        .ask(true)
                        .send_file(&file.as_fd())
                        .await?
                        .response()?;
                    Ok::<(), ashpd::Error>(())
                })?;
                Ok(())
            })();

            let message = match result {
                Ok(()) => format!("Opened application chooser for {label}"),
                Err(e) => format!("Could not open application chooser for {label}: {e}"),
            };
            let _ = tx.send(message);
            repaint.request_repaint();
        });
    }

    fn open_network_uri(&mut self, uri: &str) {
        let uri = uri.trim();
        if uri.is_empty() {
            return;
        }
        match Command::new("xdg-open").arg(uri).spawn() {
            Ok(_) => {
                self.network_history.retain(|x| x != uri);
                self.network_history.insert(0, uri.to_string());
                self.network_history.truncate(16);
                config::save_network_history(&self.network_history);
                self.status = format!("Opened network location {uri}");
            }
            Err(e) => self.status = format!("Could not open network location: {e}"),
        }
    }

    fn open_new_window(&mut self, path: &Path) {
        let p = path.to_string_lossy().to_string();
        match Self::spawn_quiet("hoard", &[&p]) {
            Ok(_) => self.status = "Opened new Hoard window".into(),
            Err(e) => self.status = format!("Could not open new window: {e}"),
        }
    }

    fn pin_folder(&mut self, path: PathBuf) {
        let target = if path.is_dir() {
            path
        } else {
            path.parent().unwrap_or(self.current_path()).to_path_buf()
        };
        if !self.pinned.contains(&target) {
            self.pinned.push(target);
            config::save_pinned(&self.pinned);
            self.status = "Folder pinned".into();
        }
    }

    fn unpin_folder(&mut self, path: &Path) {
        self.pinned.retain(|p| p != path);
        config::save_pinned(&self.pinned);
        self.status = "Folder unpinned".into();
    }

    fn open_terminal(&mut self, path: &Path) {
        match Command::new(&self.settings.terminal)
            .arg("--working-directory")
            .arg(path)
            .spawn()
        {
            Ok(_) => self.status = format!("Opened {}", self.settings.terminal),
            Err(e) => self.status = format!("Could not open terminal: {e}"),
        }
    }

    fn open_admin_terminal(&mut self, path: &Path) {
        let cmd = format!("cd {} && exec bash", shell_quote(path));
        match Command::new(&self.settings.terminal)
            .args(["-e", "pkexec", "bash", "-lc", &cmd])
            .spawn()
        {
            Ok(_) => self.status = "Requested administrator terminal".into(),
            Err(e) => self.status = format!("Could not request administrator terminal: {e}"),
        }
    }

    fn check_updates(&mut self) {
        if self.update_rx.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.update_rx = Some(rx);
        let repaint = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let msg = match updater::check_for_update() {
                Ok(Some(u)) => format!(
                    "GitHub update {} available — run: hoard --update",
                    u.version
                ),
                Ok(None) => "Hoard is up to date".into(),
                Err(e) => format!("Update check: {e}"),
            };
            if tx.send(msg).is_ok() {
                repaint.request_repaint();
            }
        });
    }

    fn apply_theme_preset_to(mut theme: Theme, preset: &str) -> Theme {
        let rgb = |r, g, b| Color32::from_rgb(r, g, b);

        let (background, panel, panel_alt, sidebar, border, foreground, muted, accent, accent_green, accent_soft, selection, selection_text, danger) =
            match preset {
                "ocean-teal" => (
                    rgb(8, 18, 24), rgb(17, 32, 40), rgb(21, 42, 50), rgb(10, 24, 31),
                    rgb(55, 84, 91), rgb(233, 250, 247), rgb(155, 187, 188), rgb(45, 165, 159),
                    rgb(116, 226, 185), rgb(23, 65, 67), rgb(23, 65, 67), rgb(255, 255, 255),
                    rgb(247, 136, 130),
                ),
                "amber-graphite" => (
                    rgb(18, 17, 16), rgb(30, 29, 27), rgb(40, 37, 32), rgb(24, 23, 22),
                    rgb(79, 70, 59), rgb(248, 239, 223), rgb(192, 180, 159), rgb(197, 134, 50),
                    rgb(153, 212, 135), rgb(69, 49, 27), rgb(69, 49, 27), rgb(18, 17, 16),
                    rgb(240, 122, 103),
                ),
                "cobalt-night" => (
                    rgb(11, 16, 30), rgb(19, 28, 45), rgb(27, 40, 61), rgb(14, 21, 35),
                    rgb(57, 77, 108), rgb(241, 246, 255), rgb(159, 184, 215), rgb(76, 131, 225),
                    rgb(118, 223, 189), rgb(28, 51, 91), rgb(28, 51, 91), rgb(255, 255, 255),
                    rgb(243, 128, 147),
                ),
                "ember-stone" => (
                    rgb(21, 16, 18), rgb(34, 25, 28), rgb(44, 33, 37), rgb(27, 20, 24),
                    rgb(84, 65, 70), rgb(253, 242, 238), rgb(199, 168, 171), rgb(206, 99, 78),
                    rgb(141, 215, 160), rgb(74, 37, 40), rgb(74, 37, 40), rgb(255, 255, 255),
                    rgb(251, 129, 135),
                ),
                "forest-carbon" => (
                    rgb(12, 18, 17), rgb(21, 30, 28), rgb(29, 42, 37), rgb(16, 23, 22),
                    rgb(57, 79, 72), rgb(234, 248, 241), rgb(161, 187, 175), rgb(93, 169, 128),
                    rgb(136, 228, 181), rgb(33, 65, 51), rgb(33, 65, 51), rgb(12, 25, 18),
                    rgb(240, 134, 130),
                ),
                "plum-rose" => (
                    rgb(19, 13, 22), rgb(32, 22, 35), rgb(44, 30, 46), rgb(25, 18, 29),
                    rgb(82, 58, 78), rgb(250, 239, 247), rgb(194, 167, 188), rgb(181, 91, 148),
                    rgb(147, 219, 181), rgb(71, 34, 61), rgb(71, 34, 61), rgb(255, 255, 255),
                    rgb(248, 132, 143),
                ),
                "ivory-ink" => (
                    rgb(230, 232, 231), rgb(252, 252, 248), rgb(237, 241, 241), rgb(220, 226, 228),
                    rgb(176, 187, 192), rgb(26, 43, 51), rgb(85, 107, 118), rgb(45, 99, 128),
                    rgb(29, 113, 87), rgb(202, 224, 233), rgb(202, 224, 233), rgb(255, 255, 255),
                    rgb(176, 54, 65),
                ),
                "solar-paper" => (
                    rgb(236, 229, 213), rgb(253, 248, 238), rgb(239, 233, 221), rgb(226, 215, 196),
                    rgb(181, 167, 149), rgb(55, 44, 35), rgb(115, 99, 85), rgb(139, 83, 42),
                    rgb(44, 123, 83), rgb(238, 210, 174), rgb(238, 210, 174), rgb(255, 255, 255),
                    rgb(173, 63, 56),
                ),
                "nord-aurora" => (
                    rgb(20, 27, 36), rgb(31, 42, 52), rgb(40, 55, 65), rgb(25, 34, 45),
                    rgb(73, 91, 105), rgb(236, 244, 249), rgb(166, 192, 207), rgb(107, 172, 194),
                    rgb(155, 217, 174), rgb(45, 83, 96), rgb(45, 83, 96), rgb(17, 39, 50),
                    rgb(236, 134, 144),
                ),
                _ => (
                    rgb(11, 13, 19), rgb(19, 22, 30), rgb(26, 30, 41), rgb(13, 16, 23),
                    rgb(57, 62, 79), rgb(239, 241, 247), rgb(162, 171, 189), rgb(139, 111, 221),
                    rgb(116, 225, 174), rgb(43, 36, 69), rgb(43, 36, 69), rgb(255, 255, 255),
                    rgb(245, 120, 133),
                ),
            };

        theme.background = background;
        theme.panel = panel;
        theme.panel_alt = panel_alt;
        theme.sidebar = sidebar;
        theme.border = border;
        theme.foreground = foreground;
        theme.muted = muted;
        theme.accent = accent;
        theme.accent_green = accent_green;
        theme.accent_soft = accent_soft;
        theme.selection = selection;
        theme.selection_text = selection_text;
        theme.danger = danger;
        theme
    }

    fn reload_theme_with_opacity(&mut self, ctx: &egui::Context) {
        self.theme = Self::apply_theme_preset_to(
            Theme::load(&config::theme_path()),
            &self.settings.theme_preset,
        )
        .with_surface_opacity(self.settings.window_opacity);
        self.theme.apply_visuals(ctx);
    }

    fn hot_reload_theme(&mut self, ctx: &egui::Context) {
        while let Ok(mt) = self.theme_check_rx.try_recv() {
            if mt != self.theme_mtime {
                self.reload_theme_with_opacity(ctx);
                self.theme_mtime = mt;
                self.status = "Theme reloaded".into();
            }
        }
    }

    fn poll_trash_count(&mut self) {
        while let Ok(count) = self.trash_count_rx.try_recv() {
            self.trash_count = count;
        }
    }

    fn save_preferences(&mut self) {
        self.settings.show_hidden = self.show_hidden;
        self.settings.default_view = if self.view == ViewMode::Icons {
            "icons".into()
        } else {
            "details".into()
        };
        self.settings.sort_column = match self.sort {
            SortColumn::Name => "name",
            SortColumn::Type => "type",
            SortColumn::Size => "size",
            SortColumn::Modified => "modified",
            SortColumn::Created => "created",
            SortColumn::Owner => "owner",
            SortColumn::Permissions => "permissions",
        }
        .into();
        self.settings.sort_ascending = self.sort_ascending;
        match config::save_settings(&self.settings) {
            Ok(_) => self.status = "Preferences updated".into(),
            Err(e) => self.status = format!("Could not save preferences: {e}"),
        }
    }

    fn keyboard(&mut self, ctx: &egui::Context) {
        let (
            ctrl,
            alt,
            shift,
            h,
            t,
            w,
            c,
            x,
            v,
            z,
            a,
            l,
            del,
            left,
            right,
            up,
            down,
            home,
            end,
            page_up,
            page_down,
            enter,
            f2,
            f5,
            esc,
            tab_key,
        ) = ctx.input(|i| {
            (
                i.modifiers.ctrl,
                i.modifiers.alt,
                i.modifiers.shift,
                i.key_pressed(egui::Key::H),
                i.key_pressed(egui::Key::T),
                i.key_pressed(egui::Key::W),
                i.key_pressed(egui::Key::C),
                i.key_pressed(egui::Key::X),
                i.key_pressed(egui::Key::V),
                i.key_pressed(egui::Key::Z),
                i.key_pressed(egui::Key::A),
                i.key_pressed(egui::Key::L),
                i.key_pressed(egui::Key::Delete),
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
                i.key_pressed(egui::Key::Home),
                i.key_pressed(egui::Key::End),
                i.key_pressed(egui::Key::PageUp),
                i.key_pressed(egui::Key::PageDown),
                i.key_pressed(egui::Key::Enter),
                i.key_pressed(egui::Key::F2),
                i.key_pressed(egui::Key::F5),
                i.key_pressed(egui::Key::Escape),
                i.key_pressed(egui::Key::Tab),
            )
        });
        let text_focus = ctx.memory(|memory| memory.focused().is_some());

        if esc {
            if self.pending_collision.is_some() {
                self.pending_collision = None;
                self.status = "Operation cancelled".into();
            } else if self.rename_target.is_some() {
                self.rename_target = None;
            } else if self.show_new_file {
                self.show_new_file = false;
            } else if self.show_new_folder {
                self.show_new_folder = false;
            } else if !self.delete_targets.is_empty() {
                self.delete_targets.clear();
            } else if self.show_preferences {
                self.show_preferences = false;
            } else if !self.search.is_empty() {
                self.search.clear();
                self.cancel_recursive_search();
                self.dirty = true;
            } else {
                self.selected = None;
                self.selected_paths.clear();
            }
        }
        if ctrl && l {
            self.location_focus_requested = true;
        }
        if ctrl && shift && z {
            self.redo_last();
        } else if ctrl && z {
            self.undo_last();
        }
        if ctrl && a && !text_focus {
            self.select_all_visible();
        }
        if !text_focus {
            if up {
                self.move_keyboard_selection(-1, shift);
            }
            if down {
                self.move_keyboard_selection(1, shift);
            }
            if home {
                self.select_keyboard_index(0, shift);
            }
            if end && !self.entries.is_empty() {
                self.select_keyboard_index(self.entries.len() - 1, shift);
            }
            if page_up {
                self.move_keyboard_selection(-10, shift);
            }
            if page_down {
                self.move_keyboard_selection(10, shift);
            }
            let entry_to_open = if enter {
                self.selected
                    .as_ref()
                    .and_then(|path| self.entries.iter().find(|entry| entry.path == *path))
                    .cloned()
            } else {
                None
            };
            if let Some(entry) = entry_to_open {
                self.open_entry(&entry);
            }
        }
        if f2 && !text_focus {
            let items = self.selected_items();
            if items.len() == 1 {
                let target = items[0].clone();
                self.rename_text = target
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_string();
                self.rename_target = Some(target);
            }
        }
        if ctrl && h {
            self.show_hidden = !self.show_hidden;
            self.dirty = true;
            self.persist_browsing_settings();
        }
        if ctrl && shift && t {
            self.reopen_closed_tab();
        } else if ctrl && t {
            let p = self.current_path().to_path_buf();
            self.new_tab(p, true);
        }
        if ctrl && w {
            self.close_tab(self.active_tab);
        }
        if ctrl && tab_key && !self.tabs.is_empty() {
            let next = if shift {
                (self.active_tab + self.tabs.len() - 1) % self.tabs.len()
            } else {
                (self.active_tab + 1) % self.tabs.len()
            };
            self.switch_to_tab(next);
        }
        if ctrl && c && !text_focus {
            let paths = self.selected_items();
            if !paths.is_empty() {
                self.clipboard = Some(ClipboardItem {
                    paths: paths.clone(),
                    mode: ClipboardMode::Copy,
                });
                self.write_system_file_clipboard(&paths, ClipboardMode::Copy);
                self.status = format!("Copied {} item(s)", paths.len());
            }
        }
        if ctrl && x && !text_focus {
            let paths = self.selected_items();
            if !paths.is_empty() {
                self.clipboard = Some(ClipboardItem {
                    paths: paths.clone(),
                    mode: ClipboardMode::Move,
                });
                self.write_system_file_clipboard(&paths, ClipboardMode::Move);
                self.status = format!("Cut {} item(s)", paths.len());
            }
        }
        if ctrl && v && !text_focus {
            let dest = self.current_path().to_path_buf();
            self.paste_into(&dest);
        }
        if del {
            let items = self.selected_items();
            if shift {
                if !items.is_empty() {
                    self.delete_targets = items;
                }
            } else {
                for p in items {
                    self.move_to_trash(&p);
                }
            }
        }
        if alt && left {
            self.go_back();
        }
        if alt && right {
            self.go_forward();
        }
        if f5 {
            self.dirty = true;
        }
    }

    fn thumbnail_for(
        &mut self,
        _ctx: &egui::Context,
        path: &Path,
        max_side: u32,
    ) -> Option<TextureHandle> {
        if !self.settings.thumbnails || !fs_model::is_visual_media_file(path) {
            return None;
        }
        let key = thumbnail::ThumbnailKey {
            path: path.to_path_buf(),
            max_side: max_side.clamp(32, 512),
            disk_cache: self.settings.thumbnail_disk_cache,
        };
        if let Some(tex) = self.thumbnails.get(&key) {
            return Some(tex.clone());
        }
        if self.thumbnail_failed.contains(&key) {
            return None;
        }
        if self.thumbnail_pending.insert(key.clone()) && !self.thumbnail_loader.request(key.clone())
        {
            self.thumbnail_pending.remove(&key);
        }
        None
    }

    fn poll_thumbnails(&mut self, ctx: &egui::Context) {
        let mut received = 0usize;
        const MAX_THUMBNAILS_PER_FRAME: usize = 3;
        let deadline = Instant::now() + Duration::from_millis(5);
        for _ in 0..MAX_THUMBNAILS_PER_FRAME {
            if Instant::now() >= deadline {
                break;
            }
            let Some(result) = self.thumbnail_loader.try_recv() else {
                break;
            };
            received += 1;
            self.thumbnail_pending.remove(&result.key);
            let still_relevant = self.entries.iter().any(|e| e.path == result.key.path)
                || self.selected.as_ref() == Some(&result.key.path);
            if !still_relevant {
                continue;
            }
            if let Some(image) = result.image {
                let tex = ctx.load_texture(
                    format!(
                        "thumb:{}:{}",
                        result.key.path.display(),
                        result.key.max_side
                    ),
                    image,
                    egui::TextureOptions::LINEAR,
                );
                self.thumbnails.insert(result.key, tex);
            } else {
                self.thumbnail_failed.insert(result.key);
            }
        }
        if self.thumbnails.len() > 768 {
            let selected = self.selected.clone();
            self.thumbnails
                .retain(|key, _| selected.as_ref() == Some(&key.path));
        }
        if received > 0 {
            ctx.request_repaint();
        }
        if received == MAX_THUMBNAILS_PER_FRAME {
            self.egui_ctx.request_repaint();
        }
    }

    fn move_to_trash(&mut self, path: &Path) {
        self.enqueue_operation(QueuedOperation::Trash(vec![path.to_path_buf()]));
    }

    fn restore_selected_from_trash(&mut self) {
        let paths: Vec<PathBuf> = self
            .selected_items()
            .into_iter()
            .filter(|path| fs_model::is_in_trash(path))
            .collect();
        if paths.is_empty() {
            self.status = "No selected Trash items to restore".into();
            return;
        }
        self.enqueue_operation(QueuedOperation::Restore(paths));
    }

    fn paste_into(&mut self, dest: &Path) {
        let item = self
            .clipboard
            .clone()
            .or_else(|| self.read_system_file_clipboard());
        let Some(item) = item else {
            self.status = "Clipboard has no files to paste".into();
            return;
        };
        if !dest.is_dir() {
            return;
        }
        self.start_file_operation(item.paths, dest.to_path_buf(), item.mode);
    }

    fn top_bar(&mut self, root: &mut egui::Ui) {
        let ctx = root.ctx().clone();
        let t = self.theme.clone();
        egui::Panel::top("title")
            .exact_size(72.0)
            .frame(
                egui::Frame::new()
                    .fill(t.sidebar)
                    .stroke(Stroke::new(1.0_f32, t.border)),
            )
            .show(root, |ui| {
                let full = ui.max_rect();
                let drag = ui.interact(full, ui.id().with("drag-title"), Sense::click_and_drag());
                if drag.drag_started() {
                    self.platform_commands
                        .push_back(PlatformCommand::BeginWindowMove);
                }

                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    self.paint_logo(ui, 42.0);
                    ui.vertical(|ui| {
                        ui.add_space(10.0);
                        ui.label(RichText::new("Hoard").strong().size(22.0));
                        ui.label(RichText::new("File Manager").size(11.0).color(t.muted));
                    });
                    ui.add_space(24.0);

                    let mut close_tab = None;
                    let mut close_others = None;
                    let mut close_right = None;
                    let mut move_left = None;
                    let mut move_right = None;
                    let mut duplicate_tab = None;
                    let mut new_window = None;

                    for idx in 0..self.tabs.len() {
                        let active = idx == self.active_tab;
                        let label = self.tabs[idx].label();
                        let path = self.tabs[idx].path.clone();
                        let fill = if active { t.panel_alt } else { t.panel };
                        let mut close_rect = None;
                        let frame_resp = egui::Frame::new()
                            .fill(fill)
                            .stroke(Stroke::new(
                                1.0_f32,
                                if active { t.accent } else { t.border },
                            ))
                            .corner_radius(7.0)
                            .inner_margin(egui::Margin::symmetric(10, 7))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let icon = if path == fs_model::trash_files_dir() {
                                        "♲"
                                    } else if path == dirs::home_dir().unwrap_or_default() {
                                        "⌂"
                                    } else {
                                        "▰"
                                    };
                                    ui.label(RichText::new(icon).color(if active {
                                        t.accent_green
                                    } else {
                                        t.accent
                                    }));
                                    ui.label(
                                        RichText::new(shorten(&label, 22))
                                            .strong()
                                            .color(t.foreground),
                                    );
                                    if self.tabs.len() > 1 {
                                        let close_resp = ui
                                            .add_sized([20.0, 20.0], egui::Button::new(""))
                                            .on_hover_text("Close tab");
                                        Self::paint_shared_glyph(
                                            ui.painter(),
                                            close_resp.rect.shrink(4.5),
                                            Glyph::Close,
                                            t.muted,
                                        );
                                        close_rect = Some(close_resp.rect);
                                        if close_resp.clicked() {
                                            close_tab = Some(idx);
                                        }
                                    }
                                });
                            })
                            .response;

                        // Keep tab activation/context interactions off the dedicated close-button hit box.
                        // A parent interaction covering the whole frame would steal clicks from the close control.
                        let mut tab_body_rect = frame_resp.rect;
                        if let Some(rect) = close_rect {
                            tab_body_rect.max.x = (rect.left() - 4.0).max(tab_body_rect.left());
                        }
                        let tab_resp = ui.interact(
                            tab_body_rect,
                            ui.id().with(("tab-body", idx)),
                            Sense::click(),
                        );

                        if tab_resp.clicked() {
                            self.switch_to_tab(idx);
                        }
                        if tab_resp.clicked_by(egui::PointerButton::Middle) && self.tabs.len() > 1 {
                            close_tab = Some(idx);
                        }
                        if tab_resp.hovered()
                            && ctx.input(|i| i.pointer.any_released())
                            && self.drag_payload.is_some()
                        {
                            self.drop_drag_on(&path, ui.input(|i| i.modifiers.ctrl));
                        }
                        tab_resp.context_menu(|ui| {
                            if ui.button("Duplicate Tab").clicked() {
                                duplicate_tab = Some(path.clone());
                                ui.close();
                            }
                            if ui.button("Open Folder in New Window").clicked() {
                                new_window = Some(path.clone());
                                ui.close();
                            }
                            if ui.button("Copy Folder Path").clicked() {
                                ui.ctx().copy_text(path.to_string_lossy().to_string());
                                ui.close();
                            }
                            ui.separator();
                            if idx > 0 && ui.button("Move Tab Left").clicked() {
                                move_left = Some(idx);
                                ui.close();
                            }
                            if idx + 1 < self.tabs.len() && ui.button("Move Tab Right").clicked() {
                                move_right = Some(idx);
                                ui.close();
                            }
                            ui.separator();
                            if self.tabs.len() > 1 && ui.button("Close Tab").clicked() {
                                close_tab = Some(idx);
                                ui.close();
                            }
                            if self.tabs.len() > 1 && ui.button("Close Other Tabs").clicked() {
                                close_others = Some(idx);
                                ui.close();
                            }
                            if idx + 1 < self.tabs.len()
                                && ui.button("Close Tabs to the Right").clicked()
                            {
                                close_right = Some(idx);
                                ui.close();
                            }
                        });
                    }

                    if let Some(path) = duplicate_tab {
                        self.new_tab(path, true);
                    }
                    if let Some(path) = new_window {
                        self.open_new_window(&path);
                    }
                    if let Some(idx) = move_left {
                        self.tabs.swap(idx, idx - 1);
                        if self.active_tab == idx {
                            self.active_tab -= 1;
                        } else if self.active_tab == idx - 1 {
                            self.active_tab += 1;
                        }
                        self.persist_session();
                    }
                    if let Some(idx) = move_right {
                        self.tabs.swap(idx, idx + 1);
                        if self.active_tab == idx {
                            self.active_tab += 1;
                        } else if self.active_tab == idx + 1 {
                            self.active_tab -= 1;
                        }
                        self.persist_session();
                    }
                    if let Some(idx) = close_right {
                        while self.tabs.len() > idx + 1 {
                            if let Some(tab) = self.tabs.pop() {
                                self.closed_tabs.push(tab);
                            }
                        }
                        self.active_tab = self.active_tab.min(self.tabs.len() - 1);
                        self.persist_session();
                        self.entries.clear();
                        self.entry_index.clear();
                        self.sync_address();
                        self.dirty = true;
                    }
                    if let Some(idx) = close_others {
                        let keep = self.tabs[idx].clone();
                        self.tabs.clear();
                        self.tabs.push(keep);
                        self.active_tab = 0;
                        self.entries.clear();
                        self.entry_index.clear();
                        self.sync_address();
                        self.dirty = true;
                        self.persist_session();
                    } else if let Some(idx) = close_tab {
                        self.close_tab(idx);
                    }

                    let new_tab_resp = ui
                        .add_sized([34.0, 34.0], egui::Button::new(""))
                        .on_hover_text("New tab  Ctrl+T");
                    Self::paint_shared_glyph(
                        ui.painter(),
                        new_tab_resp.rect.shrink(8.0),
                        Glyph::Add,
                        t.accent,
                    );
                    if new_tab_resp.clicked() {
                        let p = self.current_path().to_path_buf();
                        self.new_tab(p, true);
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add_space(8.0);
                        let decorations = WindowDecorations::default();
                        let extent = decorations.button_extent;
                        let close_resp = ui
                            .add_sized([extent, extent], egui::Button::new(""))
                            .on_hover_text("Close");
                        Self::paint_shared_glyph(
                            ui.painter(),
                            close_resp.rect.shrink(7.0),
                            Glyph::Close,
                            t.foreground,
                        );
                        if close_resp.clicked() {
                            self.platform_commands.push_back(PlatformCommand::Close);
                        }
                        let maximize_resp = ui
                            .add_sized([extent, extent], egui::Button::new(""))
                            .on_hover_text(if self.maximized {
                                "Restore"
                            } else {
                                "Maximize"
                            });
                        Self::paint_shared_glyph(
                            ui.painter(),
                            maximize_resp.rect.shrink(7.0),
                            Glyph::Window,
                            t.foreground,
                        );
                        if maximize_resp.clicked() {
                            self.maximized = !self.maximized;
                            self.platform_commands
                                .push_back(PlatformCommand::SetMaximized(self.maximized));
                        }
                        let minimize_resp = ui
                            .add_sized([extent, extent], egui::Button::new(""))
                            .on_hover_text("Minimize");
                        let min_rect = minimize_resp.rect.shrink(8.0);
                        ui.painter().line_segment(
                            [
                                Pos2::new(min_rect.left(), min_rect.center().y),
                                Pos2::new(min_rect.right(), min_rect.center().y),
                            ],
                            Stroke::new(1.8, t.foreground),
                        );
                        if minimize_resp.clicked() {
                            self.platform_commands.push_back(PlatformCommand::Minimize);
                        }
                    });
                });
            });
    }

    fn paint_shared_glyph(p: &egui::Painter, rect: Rect, glyph: Glyph, color: Color32) {
        let side = rect.width().min(rect.height());
        let origin = Pos2::new(rect.center().x - side * 0.5, rect.center().y - side * 0.5);
        for primitive in glyph_primitives(glyph) {
            match *primitive {
                GlyphPrimitive::Line {
                    x1,
                    y1,
                    x2,
                    y2,
                    width,
                } => {
                    p.line_segment(
                        [
                            Pos2::new(origin.x + x1 * side, origin.y + y1 * side),
                            Pos2::new(origin.x + x2 * side, origin.y + y2 * side),
                        ],
                        Stroke::new((width * side).max(1.0), color),
                    );
                }
                GlyphPrimitive::Circle { cx, cy, r, width } => {
                    p.circle_stroke(
                        Pos2::new(origin.x + cx * side, origin.y + cy * side),
                        r * side,
                        Stroke::new((width * side).max(1.0), color),
                    );
                }
                GlyphPrimitive::Rect {
                    x,
                    y,
                    w,
                    h,
                    radius,
                    width,
                } => {
                    p.rect_stroke(
                        Rect::from_min_size(
                            Pos2::new(origin.x + x * side, origin.y + y * side),
                            Vec2::new(w * side, h * side),
                        ),
                        radius * side,
                        Stroke::new((width * side).max(1.0), color),
                        egui::StrokeKind::Middle,
                    );
                }
            }
        }
    }

    fn paint_tool_icon(p: &egui::Painter, rect: Rect, icon: ToolIcon, color: Color32) {
        let shared = match icon {
            ToolIcon::Back => Some(Glyph::Back),
            ToolIcon::Forward => Some(Glyph::Forward),
            ToolIcon::Search => Some(Glyph::Search),
            ToolIcon::Menu => Some(Glyph::Menu),
            _ => None,
        };
        if let Some(glyph) = shared {
            Self::paint_shared_glyph(p, rect, glyph, color);
            return;
        }
        let c = rect.center();
        let w = rect.width().min(rect.height()) * 0.58;
        let s = Stroke::new(2.0_f32, color);
        match icon {
            ToolIcon::Up => {
                p.line_segment(
                    [
                        Pos2::new(c.x, c.y + w * 0.35),
                        Pos2::new(c.x, c.y - w * 0.28),
                    ],
                    s,
                );
                p.line_segment(
                    [
                        Pos2::new(c.x, c.y - w * 0.28),
                        Pos2::new(c.x - w * 0.28, c.y),
                    ],
                    s,
                );
                p.line_segment(
                    [
                        Pos2::new(c.x, c.y - w * 0.28),
                        Pos2::new(c.x + w * 0.28, c.y),
                    ],
                    s,
                );
            }
            ToolIcon::Refresh => {
                p.circle_stroke(c, w * 0.34, s);
                p.line_segment(
                    [
                        Pos2::new(c.x + w * 0.18, c.y - w * 0.33),
                        Pos2::new(c.x + w * 0.39, c.y - w * 0.32),
                    ],
                    s,
                );
                p.line_segment(
                    [
                        Pos2::new(c.x + w * 0.39, c.y - w * 0.32),
                        Pos2::new(c.x + w * 0.34, c.y - w * 0.11),
                    ],
                    s,
                );
            }
            ToolIcon::Home => {
                let roof = [
                    Pos2::new(c.x - w * 0.40, c.y - w * 0.02),
                    Pos2::new(c.x, c.y - w * 0.36),
                    Pos2::new(c.x + w * 0.40, c.y - w * 0.02),
                ];
                p.add(egui::Shape::line(roof.to_vec(), s));
                p.rect_stroke(
                    Rect::from_min_max(
                        Pos2::new(c.x - w * 0.28, c.y - w * 0.02),
                        Pos2::new(c.x + w * 0.28, c.y + w * 0.34),
                    ),
                    1.5,
                    s,
                    egui::StrokeKind::Middle,
                );
            }
            ToolIcon::Details => {
                for y in [-0.27_f32, 0.0, 0.27] {
                    p.rect_filled(
                        Rect::from_center_size(
                            Pos2::new(c.x - w * 0.34, c.y + w * y),
                            Vec2::splat(3.2),
                        ),
                        1.0,
                        color,
                    );
                    p.line_segment(
                        [
                            Pos2::new(c.x - w * 0.17, c.y + w * y),
                            Pos2::new(c.x + w * 0.38, c.y + w * y),
                        ],
                        s,
                    );
                }
            }
            ToolIcon::Icons => {
                for (dx, dy) in [
                    (-0.22_f32, -0.22_f32),
                    (0.22, -0.22),
                    (-0.22, 0.22),
                    (0.22, 0.22),
                ] {
                    p.rect_stroke(
                        Rect::from_center_size(
                            Pos2::new(c.x + w * dx, c.y + w * dy),
                            Vec2::splat(w * 0.28),
                        ),
                        2.0,
                        s,
                        egui::StrokeKind::Middle,
                    );
                }
            }
            _ => unreachable!("shared tool icon handled above"),
        }
    }

    fn toolbar_button(
        ui: &mut egui::Ui,
        icon: ToolIcon,
        label: &str,
        labels: bool,
        selected: bool,
        t: &Theme,
    ) -> egui::Response {
        let width = if labels {
            (label.chars().count() as f32 * 7.1 + 39.0).max(62.0)
        } else {
            34.0
        };
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 34.0), Sense::click());
        let fill = if selected {
            Self::darken_surface(t.panel_alt, 0.68)
        } else if response.hovered() {
            Self::darken_surface(t.panel_alt, 0.82)
        } else {
            Color32::TRANSPARENT
        };

        let stroke = if selected {
            t.accent
        } else if response.hovered() {
            t.border
        } else {
            Color32::TRANSPARENT
        };

        ui.painter().rect_filled(rect, 7.0, fill);

        if stroke != Color32::TRANSPARENT {
            ui.painter().rect_stroke(
                rect,
                7.0,
                Stroke::new(if selected { 1.4 } else { 1.0 }, stroke),
                egui::StrokeKind::Middle,
            );
        }

        if selected {
            let accent_y = rect.bottom() - 2.5;
            ui.painter().line_segment(
                [
                    Pos2::new(rect.left() + 8.0, accent_y),
                    Pos2::new(rect.right() - 8.0, accent_y),
                ],
                Stroke::new(2.0, t.accent_green),
            );
        }
        let icon_rect = if labels {
            Rect::from_min_size(
                Pos2::new(rect.left() + 7.0, rect.top() + 7.0),
                Vec2::splat(20.0),
            )
        } else {
            Rect::from_center_size(rect.center(), Vec2::splat(20.0))
        };
        Self::paint_tool_icon(
            ui.painter(),
            icon_rect,
            icon,
            Self::chrome_icon_color(t, selected || response.hovered()),
        );
        if labels {
            ui.painter().text(
                Pos2::new(rect.left() + 32.0, rect.center().y),
                Align2::LEFT_CENTER,
                label,
                FontId::proportional(13.5),
                if selected {
                    t.selection_text
                } else {
                    t.foreground
                },
            );
        }
        response
    }

    fn begin_new_file(&mut self, default_name: &str) {
        self.show_new_file = true;
        self.new_file_name = default_name.to_string();
    }

    fn new_item_menu(&mut self, ui: &mut egui::Ui) {
        ui.menu_button("File", |ui| {
            if ui.button("Text File  (.txt)").clicked() {
                self.begin_new_file("New Text File.txt");
                ui.close();
            }
            if ui.button("Blank File").clicked() {
                self.begin_new_file("New File");
                ui.close();
            }
            ui.separator();
            if ui.button("Markdown Document  (.md)").clicked() {
                self.begin_new_file("New Document.md");
                ui.close();
            }
            if ui.button("JSON File  (.json)").clicked() {
                self.begin_new_file("New File.json");
                ui.close();
            }
            if ui.button("Shell Script  (.sh)").clicked() {
                self.begin_new_file("New Script.sh");
                ui.close();
            }
            if ui.button("Python Script  (.py)").clicked() {
                self.begin_new_file("New Script.py");
                ui.close();
            }
        });
        if ui.button("Folder").clicked() {
            self.show_new_folder = true;
            self.new_folder_name = "New Folder".into();
            ui.close();
        }
    }

    fn folder_background_context_menu(&mut self, ui: &mut egui::Ui) {
        if ui.button("Open in New Window").clicked() {
            let path = self.current_path().to_path_buf();
            self.open_new_window(&path);
            ui.close();
        }
        if ui.button("Open in New Tab").clicked() {
            let path = self.current_path().to_path_buf();
            self.new_tab(path, true);
            ui.close();
        }
        ui.separator();
        ui.menu_button("New", |ui| self.new_item_menu(ui));
        ui.menu_button("Selection", |ui| self.selection_context_menu(ui));
        ui.menu_button("Sort", |ui| self.sort_context_menu(ui));
        if self.view == ViewMode::Details {
            ui.menu_button("Columns", |ui| self.columns_context_menu(ui));
        }
        if ui.button("Paste").clicked() {
            let dest = self.current_path().to_path_buf();
            self.paste_into(&dest);
            ui.close();
        }
        ui.separator();
        if ui.button("Refresh").clicked() {
            self.dirty = true;
            ui.close();
        }
        if ui.button("Open in Terminal").clicked() {
            let path = self.current_path().to_path_buf();
            self.open_terminal(&path);
            ui.close();
        }
        if self.current_path() != Path::new("/") && !self.in_trash() {
            let path = self.current_path().to_path_buf();
            if self.pinned.contains(&path) {
                if ui.button("Unpin Current Folder").clicked() {
                    self.unpin_folder(&path);
                    ui.close();
                }
            } else if ui.button("Pin Current Folder").clicked() {
                self.pin_folder(path);
                ui.close();
            }
        }
    }

    fn toolbar(&mut self, root: &mut egui::Ui) {
        let t = self.theme.clone();
        let toolbar_fill = Self::darken_surface(t.panel, 0.74);
        egui::Panel::top("toolbar")
            .exact_size(54.0)
            .frame(
                egui::Frame::new()
                    .fill(toolbar_fill)
                    .stroke(Stroke::new(1.0_f32, t.border))
                    .inner_margin(egui::Margin::symmetric(10, 7)),
            )
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;

                    // Navigation cluster.
                    egui::Frame::new()
                        .fill(t.panel_alt)
                        .stroke(Stroke::new(1.0_f32, t.border))
                        .corner_radius(8.0)
                        .inner_margin(egui::Margin::same(2))
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            if Self::toolbar_button(ui, ToolIcon::Back, "Back", false, false, &t)
                                .on_hover_text("Back  Alt+Left")
                                .clicked()
                            {
                                self.go_back();
                            }
                            if Self::toolbar_button(
                                ui,
                                ToolIcon::Forward,
                                "Forward",
                                false,
                                false,
                                &t,
                            )
                            .on_hover_text("Forward  Alt+Right")
                            .clicked()
                            {
                                self.go_forward();
                            }
                            if Self::toolbar_button(ui, ToolIcon::Up, "Up", false, false, &t)
                                .on_hover_text("Up one level")
                                .clicked()
                            {
                                self.go_up();
                            }
                            if Self::toolbar_button(
                                ui,
                                ToolIcon::Refresh,
                                "Refresh",
                                false,
                                false,
                                &t,
                            )
                            .on_hover_text("Refresh  F5")
                            .clicked()
                            {
                                self.dirty = true;
                            }
                            let home = dirs::home_dir().unwrap_or_default();
                            if Self::toolbar_button(ui, ToolIcon::Home, "Home", false, false, &t)
                                .on_hover_text("Home")
                                .clicked()
                            {
                                self.navigate(home);
                            }
                        });

                    let labels = self.settings.toolbar_labels && ui.available_width() >= 1560.0;

                    // View cluster.
                    egui::Frame::new()
                        .fill(t.panel_alt)
                        .stroke(Stroke::new(1.0_f32, t.border))
                        .corner_radius(8.0)
                        .inner_margin(egui::Margin::same(2))
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.x = 1.0;
                            if Self::toolbar_button(
                                ui,
                                ToolIcon::Details,
                                "Details",
                                labels,
                                self.view == ViewMode::Details,
                                &t,
                            )
                            .clicked()
                            {
                                self.view = ViewMode::Details;
                                self.persist_browsing_settings();
                                self.save_active_tab_state();
                                self.persist_session();
                                self.start_metadata_load();
                            }
                            if Self::toolbar_button(
                                ui,
                                ToolIcon::Icons,
                                "Icons",
                                labels,
                                self.view == ViewMode::Icons,
                                &t,
                            )
                            .clicked()
                            {
                                self.view = ViewMode::Icons;
                                self.persist_browsing_settings();
                                self.save_active_tab_state();
                                self.persist_session();
                            }
                        });


                    // Size the remainder from the row width that is actually available after
                    // the navigation cluster.  The previous window-width estimate under-counted
                    // the right-side controls, so Hidden/Sort/Menu could overlap as the window
                    // narrowed.  Collapse those controls before sacrificing the editable fields.
                    let total = ui.available_width();
                    let compact = total < 1400.0;
                    let search_w = if total >= 1400.0 {
                        220.0
                    } else if total >= 1050.0 {
                        180.0
                    } else if total >= 760.0 {
                        140.0
                    } else {
                        100.0
                    };
                    let fixed_commands = if labels {
                        310.0
                    } else if compact {
                        145.0
                    } else {
                        215.0
                    };
                    let min_addr = if total >= 760.0 { 140.0 } else { 100.0 };
                    let addr_budget = (total - search_w - fixed_commands).max(min_addr);
                    let addr_w = addr_budget.min(680.0);

                    let response = ui.add_sized(
                        [addr_w, 36.0],
                        Self::singleline_text_edit(&mut self.address)
                            .id_source("hoard-location-bar")
                            .hint_text("Location"),
                    );
                    if self.location_focus_requested {
                        response.request_focus();
                        self.location_focus_requested = false;
                    }
                    if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        let raw = self.address.trim().to_string();
                        if raw.contains("://") {
                            self.open_network_uri(&raw);
                        } else {
                            self.navigate(PathBuf::from(raw));
                        }
                    }

                    let search_response = ui.add_sized(
                        [search_w, 36.0],
                        Self::singleline_text_edit(&mut self.search).hint_text(
                            if self.recursive_search {
                                "Recursive search"
                            } else {
                                "Filter folder"
                            },
                        ),
                    );
                    if search_response.changed() {
                        self.save_active_tab_state();
                        self.persist_session();
                        if self.recursive_search {
                            if self.search.trim().is_empty() {
                                self.cancel_recursive_search();
                                self.dirty = true;
                            } else {
                                self.start_recursive_search();
                            }
                        }
                    }
                    let search_menu = ui.menu_button("   ", |ui| {
                        let mut changed = false;
                        changed |= ui
                            .checkbox(&mut self.recursive_search, "Recursive search")
                            .changed();
                        ui.separator();
                        ui.label(RichText::new("Optional filters").strong());
                        ui.horizontal(|ui| {
                            ui.label("Type/ext");
                            changed |= ui
                                .add_sized(
                                    [110.0, 24.0],
                                    Self::singleline_text_edit(&mut self.search_type_filter)
                                        .hint_text("png, rs, pdf"),
                                )
                                .changed();
                        });
                        ui.horizontal(|ui| {
                            ui.label("Min MB");
                            changed |= ui
                                .add_sized(
                                    [90.0, 24.0],
                                    Self::singleline_text_edit(&mut self.search_min_size_mb)
                                        .hint_text("0"),
                                )
                                .changed();
                        });
                        ui.horizontal(|ui| {
                            ui.label("Age days");
                            changed |= ui
                                .add_sized(
                                    [90.0, 24.0],
                                    Self::singleline_text_edit(&mut self.search_max_age_days)
                                        .hint_text("any"),
                                )
                                .changed();
                        });
                        if changed {
                            self.settings.recursive_search_default = self.recursive_search;
                            let _ = config::save_settings(&self.settings);
                            if self.recursive_search && !self.search.trim().is_empty() {
                                self.start_recursive_search();
                            } else {
                                self.cancel_recursive_search();
                                self.dirty = true;
                            }
                        }
                    });
                    let search_icon_rect = Rect::from_center_size(
                        search_menu.response.rect.center(),
                        Vec2::splat(19.0),
                    );
                    Self::paint_tool_icon(
                        ui.painter(),
                        search_icon_rect,
                        ToolIcon::Search,
                        if self.recursive_search {
                            t.accent_green
                        } else {
                            t.accent
                        },
                    );
                    if self.recursive_search {
                        ui.painter().circle_filled(
                            Pos2::new(
                                search_menu.response.rect.right() - 6.0,
                                search_menu.response.rect.top() + 7.0,
                            ),
                            2.5,
                            t.accent_green,
                        );
                    }

                    // Right-edge actions consume the remaining row width so the toolbar
                    // visually spans the window instead of ending in a dead strip.
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let menu_button =
                            ui.menu_button(if compact { "   " } else { "    Menu" }, |ui| {
                                let current = self.current_path().to_path_buf();

                                if ui.button("Open Current Folder in New Tab").clicked() {
                                    self.new_tab(current.clone(), true);
                                    ui.close();
                                }
                                if ui.button("Open Current Folder in New Window").clicked() {
                                    self.open_new_window(&current);
                                    ui.close();
                                }
                                ui.menu_button("New", |ui| self.new_item_menu(ui));

                                ui.separator();
                                let undo_label = self
                                    .undo_history
                                    .next_label()
                                    .unwrap_or_else(|| "Undo".into());
                                if ui
                                    .add_enabled(
                                        self.undo_history.next_label().is_some()
                                            && !self.file_op_busy,
                                        egui::Button::new(format!("{undo_label}    Ctrl+Z")),
                                    )
                                    .clicked()
                                {
                                    self.undo_last();
                                    ui.close();
                                }
                                let redo_label = self
                                    .undo_history
                                    .next_redo_label()
                                    .unwrap_or_else(|| "Redo".into());
                                if ui
                                    .add_enabled(
                                        self.undo_history.next_redo_label().is_some()
                                            && !self.file_op_busy,
                                        egui::Button::new(format!("{redo_label}    Ctrl+Shift+Z")),
                                    )
                                    .clicked()
                                {
                                    self.redo_last();
                                    ui.close();
                                }
                                ui.separator();
                                if ui.button("Open in Terminal").clicked() {
                                    self.open_terminal(&current);
                                    ui.close();
                                }
                                if self.pinned.iter().any(|p| p == &current) {
                                    if ui.button("Unpin Current Folder").clicked() {
                                        self.unpin_folder(&current);
                                        ui.close();
                                    }
                                } else if ui.button("Pin Current Folder").clicked() {
                                    self.pin_folder(current.clone());
                                    ui.close();
                                }
                                ui.separator();
                                if !self.operation_errors.is_empty() {
                                    ui.menu_button(
                                        format!(
                                            "Operation Errors ({})",
                                            self.operation_errors.len()
                                        ),
                                        |ui| {
                                            for err in self.operation_errors.iter().rev().take(8) {
                                                ui.add(
                                                    egui::Label::new(
                                                        RichText::new(err)
                                                            .size(11.5)
                                                            .color(t.danger),
                                                    )
                                                    .wrap(),
                                                );
                                            }
                                            if ui.button("Clear Errors").clicked() {
                                                self.operation_errors.clear();
                                                ui.close();
                                            }
                                        },
                                    );
                                }
                                if ui.button("Preferences…").clicked() {
                                    self.show_preferences = true;
                                    ui.close();
                                }
                                if ui.button("Check GitHub for Updates").clicked() {
                                    self.check_updates();
                                    ui.close();
                                }
                                if self.in_trash() {
                                    ui.separator();
                                    if ui
                                        .button(RichText::new("Empty Trash…").color(t.danger))
                                        .clicked()
                                    {
                                        self.empty_trash_confirm = true;
                                        ui.close();
                                    }
                                }
                                ui.separator();
                                ui.label(
                                    RichText::new(format!("Hoard {}", env!("CARGO_PKG_VERSION")))
                                        .color(t.muted),
                                );
                            });

                        let menu_icon_rect = Rect::from_min_size(
                            Pos2::new(
                                menu_button.response.rect.left() + 7.0,
                                menu_button.response.rect.center().y - 9.0,
                            ),
                            Vec2::splat(18.0),
                        );
                        Self::paint_tool_icon(
                            ui.painter(),
                            menu_icon_rect,
                            ToolIcon::Menu,
                            t.accent,
                        );

                        if self.file_op_busy {
                            ui.spinner();
                        }
                    });
                });
            });
    }

    fn sidebar(&mut self, root: &mut egui::Ui) {
        let t = self.theme.clone();
        let sidebar_fill = Self::darken_surface(t.sidebar, 0.76);
        egui::Panel::left("sidebar")
            .exact_size(t.metrics.sidebar_width)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    // The SidePanel frame itself must carry the translucent surface.
                    // Painting only inside the child Ui leaves the scroll/frame gutter
                    // outside that clip fully transparent and creates a desktop-colored seam.
                    .fill(sidebar_fill)
                    .stroke(Stroke::new(1.0_f32, t.border))
                    .inner_margin(egui::Margin::symmetric(10, 0)),
            )
            .show(root, |ui| {
                // Reinforce the child region with the same surface color. The enclosing
                // frame above covers margins/gutters that are outside this Ui clip.
                ui.painter().rect_filled(ui.max_rect(), 0.0, sidebar_fill);
                egui::ScrollArea::vertical()
                    .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_min_width((t.metrics.sidebar_width - 21.0).max(120.0));
                        ui.add_space(12.0);
                        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));

                        self.sidebar_path(ui, "⌂", "Home", home.clone());

                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            section_label(ui, "Pinned", t.muted);
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.label(
                                    RichText::new(self.pinned.len().to_string())
                                        .size(11.0)
                                        .color(t.muted),
                                );
                            });
                        });
                        ui.add_space(4.0);
                        egui::Frame::new()
                            .fill(t.panel)
                            .stroke(Stroke::NONE)
                            .corner_radius(8.0)
                            .inner_margin(egui::Margin::same(4))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                if self.pinned.is_empty() {
                                    ui.label(
                                        RichText::new(
                                            "Pin folders from the context menu for quick access.",
                                        )
                                        .size(12.0)
                                        .color(t.muted),
                                    );
                                } else {
                                    for path in self.pinned.clone() {
                                        let label = path
                                            .file_name()
                                            .and_then(|s| s.to_str())
                                            .unwrap_or("Pinned folder")
                                            .to_string();
                                        self.sidebar_path(ui, "★", &label, path);
                                    }
                                }
                            });

                        ui.add_space(14.0);
                        section_label(ui, "Places", t.muted);
                        ui.add_space(2.0);
                        let common = [
                            ("▣", "Desktop", home.join("Desktop")),
                            ("▤", "Documents", home.join("Documents")),
                            ("♪", "Music", home.join("Music")),
                            ("▧", "Pictures", home.join("Pictures")),
                            ("▶", "Videos", home.join("Videos")),
                            ("▰", "Projects", home.join("Projects")),
                            ("♜", "Games", home.join("Games")),
                        ];
                        for (icon, label, path) in common {
                            self.sidebar_path(ui, icon, label, path);
                        }
                        let trash_label = format!("Trash ({})", self.trash_count);
                        self.sidebar_path(ui, "♲", &trash_label, fs_model::trash_files_dir());

                        ui.add_space(14.0);
                        section_label(ui, "Devices", t.muted);
                        ui.add_space(2.0);
                        self.sidebar_path(ui, "▣", "Tiamat (/)", PathBuf::from("/"));
                        for (label, path) in self.mounts.clone() {
                            self.sidebar_path(ui, "▣", &label, path);
                        }
                        for (label, device) in self.unmounted_devices.clone() {
                            self.sidebar_unmounted_device(ui, &label, &device);
                        }

                        ui.add_space(14.0);
                        section_label(ui, "Network", t.muted);
                        ui.add_space(2.0);
                        self.sidebar_external(ui, "◎", "Network", "network:///");
                        self.sidebar_external(ui, "▰", "Windows Shares", "smb:///");
                        self.sidebar_external(ui, "▰", "SSH Locations", "sftp:///");
                        for uri in self.network_history.clone().into_iter().take(5) {
                            let label = shorten(&uri, 27);
                            self.sidebar_external(ui, "↗", &label, &uri);
                        }
                        ui.add_space(12.0);
                    });
            });
    }

    fn sidebar_path(&mut self, ui: &mut egui::Ui, icon: &str, label: &str, path: PathBuf) {
        let active = self.current_path() == path;
        let pinned = self.pinned.contains(&path);
        let fill = if active {
            self.theme.selection
        } else if pinned {
            self.theme.panel_alt
        } else {
            Color32::TRANSPARENT
        };
        let stroke = if active {
            self.theme.accent
        } else if pinned {
            self.theme.border
        } else {
            Color32::TRANSPARENT
        };
        let row_width = ui.available_width();
        let resp = egui::Frame::new()
            .fill(fill)
            .stroke(Stroke::new(1.0_f32, stroke))
            .corner_radius(if pinned { 4.0 } else { 7.0 })
            .inner_margin(egui::Margin::symmetric(9, 7))
            .show(ui, |ui| {
                if pinned {
                    // Pinned destinations are navigation rows, not compact chips. Reserve the
                    // full sidebar width so every pinned item reads as an aligned list entry.
                    ui.set_min_width((row_width - 18.0).max(1.0));
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new(icon).color(if active {
                        self.theme.accent_green
                    } else {
                        self.theme.accent
                    }));
                    ui.label(RichText::new(label).color(if active {
                        self.theme.selection_text
                    } else {
                        self.theme.foreground
                    }));
                });
            })
            .response
            .interact(Sense::click());
        if resp.clicked() {
            self.navigate(path.clone());
        }
        let can_accept_drop = self
            .drag_payload
            .as_ref()
            .map(|p| !p.paths.contains(&path))
            .unwrap_or(false);
        let drop_hovered = can_accept_drop && resp.hovered();
        if drop_hovered {
            ui.painter().rect_stroke(
                resp.rect,
                7.0,
                Stroke::new(1.5_f32, self.theme.accent_green),
                egui::StrokeKind::Middle,
            );
        }
        if drop_hovered && ui.input(|i| i.pointer.any_released()) {
            self.drop_drag_on(&path, ui.input(|i| i.modifiers.ctrl));
        }
        let is_device = self.mounts.iter().any(|(_, p)| p == &path);
        resp.context_menu(|ui| {
            if ui.button("Open").clicked() {
                self.navigate(path.clone());
                ui.close();
            }
            if ui.button("Open in New Tab").clicked() {
                self.new_tab(path.clone(), true);
                ui.close();
            }
            if ui.button("Open in New Window").clicked() {
                self.open_new_window(&path);
                ui.close();
            }
            if self.clipboard.is_some() && ui.button("Paste Into").clicked() {
                self.paste_into(&path);
                ui.close();
            }
            if path == fs_model::trash_files_dir() {
                ui.separator();
                if ui
                    .button(RichText::new("Empty Trash…").color(self.theme.danger))
                    .clicked()
                {
                    self.empty_trash_confirm = true;
                    ui.close();
                }
            } else if self.pinned.contains(&path) {
                if ui.button("Unpin Folder").clicked() {
                    self.unpin_folder(&path);
                    ui.close();
                }
            } else if path.as_path() != Path::new("/") && ui.button("Pin Folder").clicked() {
                self.pin_folder(path.clone());
                ui.close();
            }
            if is_device {
                ui.separator();
                if ui.button("Unmount").clicked() {
                    match fs_model::unmount_path(&path) {
                        Ok(_) => {
                            self.status = "Device unmounted".into();
                            self.dirty = true;
                        }
                        Err(e) => self.status = format!("Unmount failed: {e}"),
                    }
                    ui.close();
                }
                if ui.button("Eject / Power Off").clicked() {
                    match fs_model::power_off_path(&path) {
                        Ok(_) => {
                            self.status = "Device powered off".into();
                            self.dirty = true;
                        }
                        Err(e) => self.status = format!("Eject failed: {e}"),
                    }
                    ui.close();
                }
            }
        });
    }

    fn sidebar_unmounted_device(&mut self, ui: &mut egui::Ui, label: &str, device: &Path) {
        let resp = ui
            .horizontal(|ui| {
                ui.label(RichText::new("◇").color(self.theme.muted));
                ui.label(RichText::new(label).color(self.theme.muted));
            })
            .response
            .interact(Sense::click());
        if resp.clicked() {
            match fs_model::mount_device(device) {
                Ok(Some(path)) => {
                    self.status = format!("Mounted {}", path.display());
                    self.dirty = true;
                }
                Ok(None) => {
                    self.status = "Device mounted".into();
                    self.dirty = true;
                }
                Err(e) => self.status = format!("Mount failed: {e}"),
            }
        }
        resp.on_hover_text(format!("Mount {}", device.display()));
    }

    fn sidebar_external(&mut self, ui: &mut egui::Ui, icon: &str, label: &str, uri: &str) {
        let r = ui
            .horizontal(|ui| {
                ui.label(RichText::new(icon).color(self.theme.accent));
                ui.label(label);
            })
            .response
            .interact(Sense::click());
        if r.clicked() {
            self.open_network_uri(uri);
        }
    }

    fn central(&mut self, root: &mut egui::Ui) {
        let t = self.theme.clone();
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Self::darken_surface(t.background, 0.72)))
            .show(root, |ui| {
                if self.show_preferences {
                    let ctx = ui.ctx().clone();
                    self.preferences_view(ui, &ctx);
                    return;
                }

                let available = ui.available_rect_before_wrap();
                let preview_w = if self.settings.show_preview {
                    // Keep enough room for the file view while allowing the inspector
                    // to grow naturally on larger windows. The theme value remains the
                    // preferred baseline instead of acting as a rigid fixed width.
                    let min_file_view = 520.0;
                    let max_preview = (available.width() - min_file_view).max(0.0);
                    if max_preview < 235.0 {
                        0.0
                    } else {
                        let preferred = t.metrics.preview_width.clamp(270.0, 380.0);
                        let proportional = (available.width() * 0.28).clamp(270.0, 380.0);
                        preferred.max(proportional).min(max_preview)
                    }
                } else {
                    0.0
                };

                let list_rect = Rect::from_min_max(
                    available.min,
                    Pos2::new(
                        (available.max.x - preview_w).max(available.min.x),
                        available.max.y,
                    ),
                );
                let background =
                    ui.interact(list_rect, ui.id().with("folder-background"), Sense::click());
                background.context_menu(|ui| self.folder_background_context_menu(ui));
                let content_pad = 12.0_f32;
                let content_rect = list_rect.shrink(content_pad);
                ui.scope_builder(egui::UiBuilder::new().max_rect(content_rect), |ui| {
                    // Directory content is inset from every edge and clipped to its own pane so
                    // scrolling can never paint under the navigation/toolbar or over the status bar.
                    ui.set_clip_rect(content_rect);
                    if self.view == ViewMode::Details {
                        self.details_view(ui);
                    } else {
                        self.icons_view(ui);
                    }
                });

                if preview_w > 0.0 {
                    let preview_rect = Rect::from_min_max(
                        Pos2::new(list_rect.max.x, available.min.y),
                        available.max,
                    );
                    ui.scope_builder(egui::UiBuilder::new().max_rect(preview_rect), |ui| {
                        self.preview_panel(ui);
                    });
                }
            });
    }

    fn details_view(&mut self, ui: &mut egui::Ui) {
        self.start_metadata_load();

        let columns = self.visible_detail_columns();
        let available_width = ui.available_width();
        let header_h = 40.0_f32;
        let row_h = self.theme.metrics.row_height.max(28.0);
        let viewport_h = (ui.available_height() - header_h).max(row_h);

        let widths: Vec<(DetailColumn, f32)> = columns
            .iter()
            .copied()
            .map(|column| (column, self.detail_column_width(column)))
            .collect();

        // Column widths are explicitly user-controlled. In particular, Name
        // must not absorb spare viewport width again after being resized.
        let grip_w = 10.0_f32;
        let detail_pad_x = 28.0_f32;

        let (header_rect, header_response) =
            ui.allocate_exact_size(Vec2::new(available_width, header_h), Sense::hover());
        header_response.context_menu(|ui| self.columns_context_menu(ui));
        ui.painter().rect_filled(
            header_rect,
            0.0,
            Self::darken_surface(self.theme.panel_alt, 0.76),
        );
        ui.painter().line_segment(
            [
                Pos2::new(header_rect.left(), header_rect.bottom()),
                Pos2::new(header_rect.right(), header_rect.bottom()),
            ],
            Stroke::new(1.0_f32, self.theme.border),
        );

        let mut x = header_rect.left() + detail_pad_x;
        for (idx, (column, width)) in widths.iter().copied().enumerate() {
            let cell =
                Rect::from_min_size(Pos2::new(x, header_rect.top()), Vec2::new(width, header_h));
            let response = ui.interact(
                cell.shrink2(Vec2::new(4.0, 4.0)),
                ui.id().with(("details-header", column.key())),
                Sense::click(),
            );
            if response.clicked() {
                self.sort_by(column.sort_column(), true);
            }
            let sort = column.sort_column();
            let header_font = FontId::proportional(14.0);
            let header_color = self.theme.foreground;
            let sort_reserved = if self.sort == sort { 24.0 } else { 8.0 };

            Self::paint_detail_text(
                ui,
                cell,
                8.0,
                sort_reserved,
                column.label(),
                header_font,
                header_color,
            );

            if self.sort == sort {
                Self::paint_sort_indicator(
                    &ui.painter().with_clip_rect(cell),
                    cell,
                    self.sort_ascending,
                    self.theme.accent,
                );
            }

            x += width;
            if idx + 1 < widths.len() {
                let grip_rect = Rect::from_min_max(
                    Pos2::new(x - grip_w * 0.5, header_rect.top()),
                    Pos2::new(x + grip_w * 0.5, header_rect.bottom()),
                );
                let grip = ui.interact(
                    grip_rect,
                    ui.id().with(("details-column-grip", column.key())),
                    Sense::drag(),
                );

                if grip.hovered() || grip.dragged() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
                }

                let grip_color = if grip.hovered() || grip.dragged() {
                    self.theme.accent
                } else {
                    self.theme.border
                };

                ui.painter().line_segment(
                    [
                        Pos2::new(x, header_rect.top() + 5.0),
                        Pos2::new(x, header_rect.bottom() - 5.0),
                    ],
                    Stroke::new(if grip.dragged() { 2.0 } else { 1.0 }, grip_color),
                );

                if grip.dragged() {
                    let delta = ui.input(|input| input.pointer.delta().x);
                    if delta.abs() > f32::EPSILON {
                        self.set_detail_column_width(column, width + delta);
                        let _ = config::save_settings(&self.settings);
                        self.egui_ctx.request_repaint();
                    }
                }

                x += grip_w;
            }
        }

        let filtered = self.filtered_indices();
        let row_count = filtered
            .as_ref()
            .map_or(self.entries.len(), |indices| indices.len());
        let saved_scroll = self
            .tabs
            .get(self.active_tab)
            .map(|t| t.scroll_y)
            .unwrap_or(0.0);

        let scroll_output = egui::ScrollArea::vertical()
            .id_salt("details-scroll")
            .vertical_scroll_offset(saved_scroll)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let content_height = (row_count as f32 * row_h).max(viewport_h);
                let (_, content_rect) =
                    ui.allocate_space(Vec2::new(available_width, content_height));
                let clip = ui.clip_rect().intersect(content_rect);
                if clip.width() <= 0.0 || clip.height() <= 0.0 {
                    return;
                }

                let (primary_pressed, primary_down, primary_released, pointer_pos, ctrl_down) = ui
                    .input(|input| {
                        (
                            input.pointer.primary_pressed(),
                            input.pointer.primary_down(),
                            input.pointer.primary_released(),
                            input.pointer.interact_pos(),
                            input.modifiers.ctrl,
                        )
                    });

                if primary_pressed {
                    let empty_press =
                        pointer_pos
                            .filter(|pos| clip.contains(*pos))
                            .is_some_and(|pos| {
                                let local_y = pos.y - content_rect.top();

                                let in_left_gutter = pos.x < content_rect.left() + detail_pad_x;
                                let in_right_gutter = pos.x > content_rect.right() - detail_pad_x;

                                in_left_gutter
                                    || in_right_gutter
                                    || local_y < 0.0
                                    || local_y >= row_count as f32 * row_h
                            });
                    if empty_press {
                        let start = pointer_pos.unwrap_or(content_rect.min);
                        let base = if ctrl_down {
                            self.selected_paths.clone()
                        } else {
                            HashSet::new()
                        };
                        if !ctrl_down {
                            self.selected = None;
                            self.selected_paths.clear();
                            self.selection_anchor = None;
                        }
                        self.drag_payload = None;
                        self.rubber_band = Some(RubberBandSelection {
                            start,
                            current: start,
                            additive: ctrl_down,
                            base,
                        });
                    }
                }

                if primary_down && self.rubber_band.is_some() {
                    if let Some((pos, state)) = pointer_pos.zip(self.rubber_band.as_mut()) {
                        state.current = pos;
                    }
                    if let Some(state) = self.rubber_band.as_ref() {
                        let selection_rect = Rect::from_min_max(
                            Pos2::new(
                                state.start.x.min(state.current.x),
                                state.start.y.min(state.current.y),
                            ),
                            Pos2::new(
                                state.start.x.max(state.current.x),
                                state.start.y.max(state.current.y),
                            ),
                        );
                        let mut selected = if state.additive {
                            state.base.clone()
                        } else {
                            HashSet::new()
                        };
                        for pos in 0..row_count {
                            let row_rect = Rect::from_min_size(
                                Pos2::new(
                                    content_rect.left() + detail_pad_x,
                                    content_rect.top() + pos as f32 * row_h,
                                ),
                                Vec2::new(
                                    (content_rect.width() - detail_pad_x * 2.0).max(1.0),
                                    row_h,
                                ),
                            );
                            if !row_rect.intersects(selection_rect) {
                                continue;
                            }
                            let entry_idx = filtered
                                .as_ref()
                                .and_then(|indices| indices.get(pos).copied())
                                .unwrap_or(pos);
                            if let Some(entry) = self.entries.get(entry_idx) {
                                selected.insert(entry.path.clone());
                            }
                        }
                        self.selected_paths = selected;
                        self.selected = self
                            .entries
                            .iter()
                            .find(|entry| self.selected_paths.contains(&entry.path))
                            .map(|entry| entry.path.clone());

                        let painter = ui.painter().with_clip_rect(clip);
                        painter.rect_filled(
                            selection_rect,
                            2.0,
                            self.theme.accent_soft.linear_multiply(0.38),
                        );
                        painter.rect_stroke(
                            selection_rect,
                            2.0,
                            Stroke::new(1.0, self.theme.accent_green),
                            egui::StrokeKind::Inside,
                        );
                    }
                }

                let first_row = (((clip.top() - content_rect.top()) / row_h).floor().max(0.0)
                    as usize)
                    .min(row_count);
                let last_row = ((((clip.bottom() - content_rect.top()) / row_h).ceil() as usize)
                    + 1)
                .min(row_count);

                for pos in first_row..last_row {
                    let entry_idx = filtered
                        .as_ref()
                        .and_then(|indices| indices.get(pos).copied())
                        .unwrap_or(pos);
                    let Some(entry) = self.entries.get(entry_idx).cloned() else {
                        continue;
                    };
                    let row_rect = Rect::from_min_size(
                        Pos2::new(
                            content_rect.left() + detail_pad_x,
                            content_rect.top() + pos as f32 * row_h,
                        ),
                        Vec2::new((content_rect.width() - detail_pad_x * 2.0).max(1.0), row_h),
                    );
                    let response = ui.interact(
                        row_rect,
                        ui.id().with(("details-row", &entry.path)),
                        Sense::click_and_drag(),
                    );
                    let selected = self.selected_paths.contains(&entry.path);
                    let can_accept_drop = entry.is_dir
                        && self
                            .drag_payload
                            .as_ref()
                            .map(|payload| !payload.paths.contains(&entry.path))
                            .unwrap_or(false);
                    let drop_hovered = can_accept_drop && response.hovered();
                    let fill = if drop_hovered {
                        self.theme.accent_soft
                    } else if selected {
                        self.theme.selection
                    } else if response.hovered() {
                        Self::darken_surface(self.theme.panel_alt, 0.88)
                    } else {
                        Color32::TRANSPARENT
                    };
                    let paint_rect = row_rect.shrink2(Vec2::new(2.0, 1.0));
                    ui.painter().rect_filled(paint_rect, 4.0, fill);
                    if drop_hovered {
                        ui.painter().rect_stroke(
                            paint_rect,
                            4.0,
                            Stroke::new(1.5_f32, self.theme.accent_green),
                            egui::StrokeKind::Middle,
                        );
                    }

                    let mut cell_x = row_rect.left();
                    for (idx, (column, width)) in widths.iter().copied().enumerate() {
                        let cell = Rect::from_min_size(
                            Pos2::new(cell_x, row_rect.top()),
                            Vec2::new(width, row_h),
                        );
                        let text_color = if selected {
                            self.theme.selection_text
                        } else {
                            self.theme.foreground
                        };
                        match column {
                            DetailColumn::Name => {
                                let icon_rect = Rect::from_min_size(
                                    Pos2::new(cell.left() + 8.0, cell.center().y - 12.0),
                                    Vec2::splat(24.0),
                                );
                                let thumb = self.thumbnail_for(ui.ctx(), &entry.path, 64);
                                if let Some(tex) = thumb {
                                    ui.put(
                                        icon_rect,
                                        egui::Image::new((tex.id(), icon_rect.size()))
                                            .corner_radius(3.0),
                                    );
                                } else if entry.is_dir {
                                    paint_folder_icon(ui.painter(), icon_rect, self.theme.accent);
                                } else {
                                    paint_typed_file(
                                        ui.painter(),
                                        icon_rect,
                                        fs_model::icon_badge(&entry),
                                        self.theme.muted,
                                        self.theme.accent,
                                    );
                                }
                                Self::paint_detail_text(
                                    ui,
                                    cell,
                                    40.0,
                                    8.0,
                                    &entry.name,
                                    FontId::proportional(14.5),
                                    text_color,
                                );
                            }
                            DetailColumn::Type => {
                                Self::paint_detail_text(
                                    ui,
                                    cell,
                                    8.0,
                                    8.0,
                                    &entry.file_type,
                                    FontId::proportional(13.5),
                                    text_color,
                                );
                            }
                            DetailColumn::Size => {
                                let value = if entry.is_dir {
                                    "—".to_string()
                                } else if entry.metadata_loaded {
                                    fs_model::format_size(entry.size)
                                } else {
                                    "…".to_string()
                                };
                                Self::paint_detail_text(
                                    ui,
                                    cell,
                                    8.0,
                                    8.0,
                                    &value,
                                    FontId::proportional(13.5),
                                    self.theme.muted,
                                );
                            }
                            DetailColumn::Modified => {
                                let value = if entry.metadata_loaded {
                                    fs_model::format_modified(entry.modified)
                                } else {
                                    "…".into()
                                };
                                Self::paint_detail_text(
                                    ui,
                                    cell,
                                    8.0,
                                    8.0,
                                    &value,
                                    FontId::proportional(13.5),
                                    text_color,
                                );
                            }
                            DetailColumn::Created => {
                                let value = if entry.metadata_loaded {
                                    fs_model::format_modified(entry.created)
                                } else {
                                    "…".into()
                                };
                                Self::paint_detail_text(
                                    ui,
                                    cell,
                                    8.0,
                                    8.0,
                                    &value,
                                    FontId::proportional(13.5),
                                    text_color,
                                );
                            }
                            DetailColumn::Owner => {
                                let value = if entry.metadata_loaded {
                                    entry
                                        .owner
                                        .map(|uid| uid.to_string())
                                        .unwrap_or_else(|| "—".into())
                                } else {
                                    "…".into()
                                };
                                Self::paint_detail_text(
                                    ui,
                                    cell,
                                    8.0,
                                    8.0,
                                    &value,
                                    FontId::proportional(13.5),
                                    text_color,
                                );
                            }
                            DetailColumn::Permissions => {
                                let value = if entry.metadata_loaded {
                                    entry
                                        .mode
                                        .map(|mode| format!("{:04o}", mode & 0o7777))
                                        .unwrap_or_else(|| "—".into())
                                } else {
                                    "…".into()
                                };
                                Self::paint_detail_text(
                                    ui,
                                    cell,
                                    8.0,
                                    8.0,
                                    &value,
                                    FontId::monospace(13.0),
                                    text_color,
                                );
                            }
                        }
                        cell_x += width;
                        if idx + 1 < widths.len() {
                            cell_x += grip_w;
                        }
                    }

                    if response.clicked() {
                        let additive = ui.input(|input| input.modifiers.ctrl);
                        self.select_entry(
                            entry.path.clone(),
                            additive,
                            ui.input(|input| input.modifiers.shift),
                        );
                    }
                    if response.double_clicked() {
                        if entry.is_dir && ui.input(|input| input.modifiers.shift) {
                            self.open_new_window(&entry.path);
                        } else {
                            self.open_entry(&entry);
                        }
                    }
                    if entry.is_dir && response.clicked_by(egui::PointerButton::Middle) {
                        self.new_tab(entry.path.clone(), false);
                        self.status = "Opened folder in background tab".into();
                    }
                    if response.drag_started() {
                        if !self.selected_paths.contains(&entry.path) {
                            self.select_entry(entry.path.clone(), false, false);
                        }
                        let paths = self.selected_items();
                        self.drag_payload = Some(DragPayload {
                            paths: paths.clone(),
                        });
                        self.platform_commands
                            .push_back(PlatformCommand::ArmFileDrag(paths));
                    }
                    if drop_hovered && ui.input(|input| input.pointer.any_released()) {
                        self.drop_drag_on(&entry.path, ui.input(|input| input.modifiers.ctrl));
                    }
                    response.context_menu(|ui| self.entry_context_menu(ui, &entry));
                }

                if primary_released && self.rubber_band.is_some() {
                    self.rubber_band = None;
                    self.save_active_tab_state();
                    self.persist_session();
                    self.start_selected_folder_stats();
                }
            });

        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.scroll_y = scroll_output.state.offset.y;
        }
        if ui.input(|input| input.pointer.any_released()) {
            self.persist_session();
        }
    }

    fn icons_view(&mut self, ui: &mut egui::Ui) {
        let filtered = self.filtered_indices();
        let item_count = filtered
            .as_ref()
            .map_or(self.entries.len(), |indices| indices.len());
        if item_count == 0 {
            self.empty_folder_state(ui);
            return;
        }

        let thumb_size = self.settings.thumbnail_size.clamp(64, 192) as f32;
        let card_w = (thumb_size + 54.0).max(164.0);
        let card_h = thumb_size + 136.0;
        let col_gap = 22.0_f32;
        let row_gap = 24.0_f32;
        let card_pad_x = 12.0_f32;
        let card_pad_y = 12.0_f32;
        let grid_pad_x = 18.0_f32;
        let grid_pad_y = 16.0_f32;
        let row_stride = card_h + row_gap;

        let saved_scroll = self
            .tabs
            .get(self.active_tab)
            .map(|t| t.scroll_y)
            .unwrap_or(0.0);
        let viewport_height = ui.available_height();
        let scroll_output = egui::ScrollArea::vertical()
            .id_salt("icon-grid-scroll")
            .vertical_scroll_offset(saved_scroll)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                // Use one coordinate space only: the actual scroll-content rectangle and the
                // actual UI clip rectangle. This avoids viewport/content-origin drift while
                // retaining visible-row culling for large directories.
                let available = ui.available_width().max(card_w + grid_pad_x * 2.0);
                let usable = (available - grid_pad_x * 2.0).max(card_w);
                let columns = (((usable + col_gap) / (card_w + col_gap)).floor() as usize).max(1);
                let rows = item_count.div_ceil(columns);
                let grid_height = grid_pad_y * 2.0
                    + rows as f32 * card_h
                    + rows.saturating_sub(1) as f32 * row_gap;
                let content_height = grid_height.max(viewport_height);

                let (_, content_rect) = ui.allocate_space(Vec2::new(available, content_height));

                // The entire visible icon-view viewport is an empty-space
                // selection surface. Card geometry still uses content_rect,
                // but whitespace to the right/below the grid must not become
                // a dead zone.
                let selection_viewport = ui.clip_rect();
                let grid_clip = selection_viewport.intersect(content_rect);

                if selection_viewport.width() <= 0.0 || selection_viewport.height() <= 0.0 {
                    return;
                }

                let (primary_pressed, primary_down, primary_released, pointer_pos, ctrl_down) = ui
                    .input(|i| {
                        (
                            i.pointer.primary_pressed(),
                            i.pointer.primary_down(),
                            i.pointer.primary_released(),
                            i.pointer.interact_pos(),
                            i.modifiers.ctrl,
                        )
                    });

                if primary_pressed {
                    let empty_press = pointer_pos
                        .filter(|pos| selection_viewport.contains(*pos))
                        .is_some_and(|pos| {
                            let local_x = pos.x - content_rect.left() - grid_pad_x;
                            let local_y = pos.y - content_rect.top() - grid_pad_y;
                            if local_x < 0.0 || local_y < 0.0 {
                                return true;
                            }
                            let cell_w = card_w + col_gap;
                            let cell_h = card_h + row_gap;
                            let col = (local_x / cell_w).floor() as usize;
                            let row = (local_y / cell_h).floor() as usize;
                            let within_card =
                                local_x % cell_w <= card_w && local_y % cell_h <= card_h;
                            let pos_idx = row.saturating_mul(columns).saturating_add(col);
                            !(within_card && pos_idx < item_count)
                        });
                    if empty_press {
                        let start = pointer_pos.unwrap_or(content_rect.min);
                        let base = if ctrl_down {
                            self.selected_paths.clone()
                        } else {
                            HashSet::new()
                        };
                        if !ctrl_down {
                            self.selected = None;
                            self.selected_paths.clear();
                            self.selection_anchor = None;
                        }
                        self.drag_payload = None;
                        self.rubber_band = Some(RubberBandSelection {
                            start,
                            current: start,
                            additive: ctrl_down,
                            base,
                        });
                    }
                }

                if primary_down && self.rubber_band.is_some() {
                    if let Some((pos, state)) = pointer_pos.zip(self.rubber_band.as_mut()) {
                        state.current = pos;
                    }
                    let selection_rect = self.rubber_band.as_ref().map(|state| {
                        Rect::from_min_max(
                            Pos2::new(
                                state.start.x.min(state.current.x),
                                state.start.y.min(state.current.y),
                            ),
                            Pos2::new(
                                state.start.x.max(state.current.x),
                                state.start.y.max(state.current.y),
                            ),
                        )
                    });
                    if let Some(selection_rect) = selection_rect {
                        self.apply_rubber_band_selection(
                            selection_rect,
                            &filtered,
                            RubberBandGridLayout {
                                content_rect,
                                item_count,
                                columns,
                                card_size: Vec2::new(card_w, card_h),
                                col_gap,
                                row_gap,
                                padding: Vec2::new(grid_pad_x, grid_pad_y),
                            },
                        );
                    }
                }

                let local_top = (grid_clip.top() - content_rect.top() - grid_pad_y).max(0.0);
                let local_bottom = (grid_clip.bottom() - content_rect.top() - grid_pad_y).max(0.0);
                let first_row = ((local_top / row_stride).floor() as usize).min(rows);
                let last_row = (((local_bottom / row_stride).ceil() as usize) + 1).min(rows);

                for row in first_row..last_row {
                    let card_top = content_rect.top() + grid_pad_y + row as f32 * row_stride;
                    for col in 0..columns {
                        let pos = row * columns + col;
                        if pos >= item_count {
                            break;
                        }
                        let entry_idx = filtered
                            .as_ref()
                            .and_then(|indices| indices.get(pos).copied())
                            .unwrap_or(pos);
                        let Some(entry) = self.entries.get(entry_idx).cloned() else {
                            break;
                        };

                        let card_left =
                            content_rect.left() + grid_pad_x + col as f32 * (card_w + col_gap);
                        let card_rect = Rect::from_min_size(
                            Pos2::new(card_left, card_top),
                            Vec2::new(card_w, card_h),
                        );
                        if !card_rect.intersects(grid_clip) {
                            continue;
                        }

                        let selected = self.selected_paths.contains(&entry.path);
                        let response = ui.interact(
                            card_rect,
                            ui.id().with(("icon-card", entry_idx)),
                            Sense::click_and_drag(),
                        );
                        let can_accept_drop = entry.is_dir
                            && self
                                .drag_payload
                                .as_ref()
                                .map(|p| !p.paths.contains(&entry.path))
                                .unwrap_or(false);
                        let drop_hovered = can_accept_drop && response.hovered();
                        let fill = if drop_hovered {
                            self.theme.accent_soft
                        } else if selected {
                            self.theme.selection
                        } else if response.hovered() {
                            self.theme.panel_alt
                        } else {
                            self.theme.panel
                        };
                        let border = if drop_hovered || selected || response.hovered() {
                            self.theme.accent
                        } else {
                            self.theme.border
                        };
                        let card_painter = ui.painter().with_clip_rect(grid_clip);
                        card_painter.rect_filled(card_rect, 9.0, fill);
                        card_painter.rect_stroke(
                            card_rect,
                            9.0,
                            Stroke::new(1.0_f32, border),
                            egui::StrokeKind::Middle,
                        );

                        let card_content = card_rect.shrink2(Vec2::new(card_pad_x, card_pad_y));
                        let footer_h = 58.0_f32;
                        let footer_rect = Rect::from_min_max(
                            Pos2::new(card_content.left(), card_content.bottom() - footer_h),
                            card_content.max,
                        );
                        let visual_slot = Rect::from_min_max(
                            card_content.min,
                            Pos2::new(card_content.max.x, footer_rect.top() - 8.0),
                        );
                        let thumb_frame = visual_slot.shrink2(Vec2::new(8.0, 6.0));
                        let frame_fill = if selected {
                            self.theme.panel_alt
                        } else {
                            self.theme.panel
                        };
                        card_painter.rect_filled(thumb_frame, 8.0, frame_fill);

                        if let Some(tex) =
                            self.thumbnail_for(ui.ctx(), &entry.path, self.settings.thumbnail_size)
                        {
                            let [tw, th] = tex.size();
                            let tex_size = Vec2::new(tw as f32, th as f32);
                            let max_size = thumb_frame.shrink2(Vec2::new(6.0, 6.0)).size();
                            let scale = (max_size.x / tex_size.x)
                                .min(max_size.y / tex_size.y)
                                .min(1.0);
                            let draw_size = Vec2::new(
                                (tex_size.x * scale).floor().max(24.0),
                                (tex_size.y * scale).floor().max(24.0),
                            );
                            let image_rect =
                                Rect::from_center_size(thumb_frame.center(), draw_size);
                            let thumb_clip = thumb_frame.intersect(grid_clip);
                            ui.scope_builder(egui::UiBuilder::new().max_rect(thumb_frame), |ui| {
                                ui.set_clip_rect(thumb_clip);
                                ui.put(
                                    image_rect,
                                    egui::Image::new((tex.id(), image_rect.size()))
                                        .corner_radius(6.0),
                                );
                            });
                        } else {
                            let painter = ui
                                .painter()
                                .with_clip_rect(thumb_frame.intersect(grid_clip));
                            let icon_side =
                                (thumb_frame.width().min(thumb_frame.height()) - 28.0).max(28.0);
                            let image_rect = Rect::from_center_size(
                                thumb_frame.center(),
                                Vec2::splat(icon_side),
                            );
                            if entry.is_dir {
                                paint_folder_icon(&painter, image_rect, self.theme.accent);
                            } else {
                                paint_typed_file(
                                    &painter,
                                    image_rect,
                                    fs_model::icon_badge(&entry),
                                    self.theme.muted,
                                    self.theme.accent,
                                );
                            }
                        }

                        let footer_fill = if selected {
                            self.theme.accent_soft
                        } else {
                            self.theme.panel_alt
                        };
                        let footer_stroke = if selected || response.hovered() {
                            self.theme.accent
                        } else {
                            self.theme.border
                        };
                        card_painter.rect_filled(footer_rect, 7.0, footer_fill);
                        card_painter.rect_stroke(
                            footer_rect,
                            7.0,
                            Stroke::new(1.0_f32, footer_stroke),
                            egui::StrokeKind::Middle,
                        );

                        let filename_rect = Rect::from_min_max(
                            Pos2::new(footer_rect.left() + 12.0, footer_rect.top() + 7.0),
                            Pos2::new(footer_rect.right() - 10.0, footer_rect.top() + 29.0),
                        );
                        let filename_capacity =
                            ((filename_rect.width() / 7.2).floor() as usize).clamp(8, 34);
                        ui.painter()
                            .with_clip_rect(filename_rect.intersect(grid_clip))
                            .text(
                                Pos2::new(filename_rect.left(), filename_rect.center().y),
                                Align2::LEFT_CENTER,
                                shorten(&entry.name, filename_capacity),
                                FontId::proportional(12.8),
                                if selected {
                                    self.theme.selection_text
                                } else {
                                    self.theme.foreground
                                },
                            );

                        let type_row = Rect::from_min_max(
                            Pos2::new(footer_rect.left() + 12.0, footer_rect.top() + 31.0),
                            Pos2::new(footer_rect.right() - 10.0, footer_rect.bottom() - 5.0),
                        );
                        let type_icon = if entry.file_type == "Image" {
                            "▧"
                        } else if entry.file_type == "Configuration" {
                            "⚙"
                        } else {
                            "•"
                        };
                        let icon_pos = Pos2::new(type_row.left(), type_row.center().y);
                        let type_painter =
                            ui.painter().with_clip_rect(type_row.intersect(grid_clip));
                        type_painter.text(
                            icon_pos,
                            Align2::LEFT_CENTER,
                            type_icon,
                            FontId::proportional(13.0),
                            self.theme.accent,
                        );
                        type_painter.text(
                            Pos2::new(type_row.left() + 24.0, type_row.center().y),
                            Align2::LEFT_CENTER,
                            &entry.file_type,
                            FontId::proportional(10.8),
                            self.theme.muted,
                        );

                        if response.clicked() {
                            let additive = ui.input(|i| i.modifiers.ctrl);
                            self.select_entry(
                                entry.path.clone(),
                                additive,
                                ui.input(|i| i.modifiers.shift),
                            );
                        }
                        if response.double_clicked() {
                            if entry.is_dir && ui.input(|i| i.modifiers.shift) {
                                self.open_new_window(&entry.path);
                            } else {
                                self.open_entry(&entry);
                            }
                        }
                        if entry.is_dir && response.clicked_by(egui::PointerButton::Middle) {
                            self.new_tab(entry.path.clone(), false);
                            self.status = "Opened folder in background tab".into();
                        }
                        if response.drag_started() {
                            if !self.selected_paths.contains(&entry.path) {
                                self.select_entry(entry.path.clone(), false, false);
                            }
                            let paths = self.selected_items();
                            self.drag_payload = Some(DragPayload {
                                paths: paths.clone(),
                            });
                            self.platform_commands
                                .push_back(PlatformCommand::ArmFileDrag(paths));
                        }
                        if drop_hovered && ui.input(|i| i.pointer.any_released()) {
                            self.drop_drag_on(&entry.path, ui.input(|i| i.modifiers.ctrl));
                        }
                        response.context_menu(|ui| self.entry_context_menu(ui, &entry));
                    }
                }

                if let Some(state) = self.rubber_band.as_ref() {
                    let selection_rect = Rect::from_min_max(
                        Pos2::new(
                            state.start.x.min(state.current.x),
                            state.start.y.min(state.current.y),
                        ),
                        Pos2::new(
                            state.start.x.max(state.current.x),
                            state.start.y.max(state.current.y),
                        ),
                    )
                    .intersect(selection_viewport);
                    if selection_rect.is_positive() {
                        let painter = ui.painter().with_clip_rect(selection_viewport);
                        painter.rect_filled(selection_rect, 3.0, self.theme.accent_soft);
                        painter.rect_stroke(
                            selection_rect,
                            3.0,
                            Stroke::new(1.0, self.theme.accent_green),
                            egui::StrokeKind::Middle,
                        );
                    }
                }

                if primary_released && self.rubber_band.take().is_some() {
                    self.selection_anchor = self.selected.clone();
                    self.save_active_tab_state();
                    self.persist_session();
                    self.start_selected_folder_stats();
                }
            });

        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.scroll_y = scroll_output.state.offset.y;
        }
        if ui.input(|i| i.pointer.any_released()) {
            self.persist_session();
        }
    }

    fn empty_folder_state(&mut self, ui: &mut egui::Ui) {
        let searching = !self.search.trim().is_empty();
        let in_trash = self.in_trash();
        let available_h = ui.available_height();
        ui.vertical_centered(|ui| {
            ui.add_space((available_h * 0.22).clamp(42.0, 150.0));
            if self.loading {
                ui.spinner();
                ui.add_space(10.0);
                ui.label(RichText::new("Loading folder…").size(17.0).strong());
                ui.label(RichText::new("Hoard is reading this location in the background.").color(self.theme.muted));
                return;
            }

            self.paint_logo(ui, 58.0);
            ui.add_space(10.0);
            if searching {
                ui.label(RichText::new("No matching items").size(18.0).strong());
                ui.add_space(3.0);
                ui.label(RichText::new(format!("Nothing in this folder matches “{}”.", self.search.trim())).color(self.theme.muted));
                ui.add_space(12.0);
                if ui.button("Clear Search").clicked() {
                    self.search.clear();
                    self.egui_ctx.request_repaint();
                }
            } else if in_trash {
                ui.label(RichText::new("Trash is empty").size(18.0).strong());
                ui.add_space(3.0);
                ui.label(RichText::new("Deleted items will appear here until they are restored or removed permanently.").color(self.theme.muted));
            } else {
                ui.label(RichText::new("This folder is empty").size(18.0).strong());
                ui.add_space(3.0);
                ui.label(RichText::new("Create something here or drop files into this window.").color(self.theme.muted));
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("＋ New Folder").clicked() {
                        self.show_new_folder = true;
                        self.new_folder_name = "New Folder".into();
                    }
                    if ui.button("＋ New Text File").clicked() {
                        self.begin_new_file("New Text File.txt");
                    }
                });
            }
        });
    }

    fn entry_context_menu(&mut self, ui: &mut egui::Ui, entry: &FileEntry) {
        let is_trash = fs_model::is_in_trash(&entry.path);
        let selection = if self.selected_paths.contains(&entry.path) {
            self.selected_items()
        } else {
            vec![entry.path.clone()]
        };
        let selection_count = selection.len();

        ui.label(
            RichText::new(&entry.name)
                .strong()
                .color(self.theme.foreground),
        );
        ui.separator();
        ui.menu_button("Selection", |ui| self.selection_context_menu(ui));
        if ui.button("Open").clicked() {
            self.open_entry(entry);
            ui.close();
        }
        if fs_model::is_image_file(&entry.path)
            && ui.button("Open with Default Application").clicked()
        {
            self.open_with_default(&entry.path, &entry.name);
            ui.close();
        }
        if !entry.is_dir && ui.button("Open With…").clicked() {
            self.open_with_chooser(&entry.path, &entry.name);
            ui.close();
        }
        if entry.is_dir {
            if ui.button("Open in New Tab").clicked() {
                self.new_tab(entry.path.clone(), true);
                ui.close();
            }
            if ui.button("Open in New Window").clicked() {
                self.open_new_window(&entry.path);
                ui.close();
            }
        } else {
            let parent_to_open = entry
                .path
                .parent()
                .filter(|_| ui.button("Open Parent in New Tab").clicked())
                .map(Path::to_path_buf);
            if let Some(parent) = parent_to_open {
                self.new_tab(parent, true);
                ui.close();
            }
        }

        ui.separator();
        let cut_label = if selection_count > 1 {
            format!("Cut {selection_count} Items")
        } else {
            "Cut".into()
        };
        let copy_label = if selection_count > 1 {
            format!("Copy {selection_count} Items")
        } else {
            "Copy".into()
        };
        if ui.button(cut_label).clicked() {
            self.clipboard = Some(ClipboardItem {
                paths: selection.clone(),
                mode: ClipboardMode::Move,
            });
            self.write_system_file_clipboard(&selection, ClipboardMode::Move);
            self.status = format!("Cut {} item(s)", selection_count);
            ui.close();
        }
        if ui.button(copy_label).clicked() {
            self.clipboard = Some(ClipboardItem {
                paths: selection.clone(),
                mode: ClipboardMode::Copy,
            });
            self.write_system_file_clipboard(&selection, ClipboardMode::Copy);
            self.status = format!("Copied {} item(s)", selection_count);
            ui.close();
        }
        if entry.is_dir && self.clipboard.is_some() && ui.button("Paste Into This Folder").clicked()
        {
            self.paste_into(&entry.path);
            ui.close();
        }
        if !is_trash && selection_count == 1 && ui.button("Duplicate").clicked() {
            self.enqueue_operation(QueuedOperation::Duplicate(entry.path.clone()));
            ui.close();
        }

        ui.separator();
        if !is_trash && selection_count == 1 && ui.button("Rename…").clicked() {
            self.rename_target = Some(entry.path.clone());
            self.rename_text = entry.name.clone();
            ui.close();
        }
        if ui.button("Copy Name").clicked() {
            ui.ctx().copy_text(entry.name.clone());
            ui.close();
        }
        if ui.button("Copy Full Path").clicked() {
            ui.ctx().copy_text(entry.path.to_string_lossy().to_string());
            ui.close();
        }
        if ui.button("Open in Terminal").clicked() {
            let p = if entry.is_dir {
                entry.path.clone()
            } else {
                entry
                    .path
                    .parent()
                    .unwrap_or(self.current_path())
                    .to_path_buf()
            };
            self.open_terminal(&p);
            ui.close();
        }

        if !is_trash {
            let pin_target = if entry.is_dir {
                entry.path.clone()
            } else {
                entry
                    .path
                    .parent()
                    .unwrap_or(self.current_path())
                    .to_path_buf()
            };
            if self.pinned.contains(&pin_target) {
                if ui.button("Remove from Pinned").clicked() {
                    self.unpin_folder(&pin_target);
                    ui.close();
                }
            } else if ui.button("Pin Folder").clicked() {
                self.pin_folder(pin_target);
                ui.close();
            }
        }

        ui.separator();
        if is_trash {
            let restore_label = if selection_count > 1 {
                "Restore Selection"
            } else {
                "Restore"
            };
            if ui.button(restore_label).clicked() {
                self.restore_selected_from_trash();
                ui.close();
            }
            if ui
                .button(RichText::new("Delete Permanently…").color(self.theme.danger))
                .clicked()
            {
                self.delete_targets = selection.clone();
                ui.close();
            }
        } else if ui
            .button(
                RichText::new(if selection_count > 1 {
                    "Move Selection to Trash"
                } else {
                    "Move to Trash"
                })
                .color(self.theme.danger),
            )
            .clicked()
        {
            for path in selection {
                self.move_to_trash(&path);
            }
            ui.close();
        }

        ui.separator();
        if ui.button("Properties / Inspector").clicked() {
            self.select_entry(entry.path.clone(), false, false);
            self.status = "Properties shown in inspector".into();
            ui.close();
        }
    }

    fn preview_panel(&mut self, ui: &mut egui::Ui) {
        let panel_rect = ui.max_rect();
        ui.painter()
            .rect_filled(panel_rect, 0.0, self.theme.sidebar);
        ui.painter().line_segment(
            [panel_rect.left_top(), panel_rect.left_bottom()],
            Stroke::new(1.0_f32, self.theme.border),
        );

        let inner_rect = panel_rect.shrink2(Vec2::new(16.0, 14.0));
        ui.scope_builder(egui::UiBuilder::new().max_rect(inner_rect), |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new("INSPECTOR")
                            .size(12.0)
                            .strong()
                            .color(self.theme.accent),
                    );
                    ui.add_space(9.0);

                    self.start_selected_folder_stats();
                    if let Some(path) = self.selected.clone() {
                        let selected_entry = self
                            .entry_index
                            .get(&path)
                            .and_then(|idx| self.entries.get(*idx))
                            .cloned();
                        let is_dir = selected_entry
                            .as_ref()
                            .map(|entry| entry.is_dir)
                            .unwrap_or(false);
                        let display_type = selected_entry
                            .as_ref()
                            .map(|entry| entry.file_type.clone())
                            .unwrap_or_else(|| {
                                if is_dir {
                                    "Folder".into()
                                } else {
                                    "File".into()
                                }
                            });
                        let badge = selected_entry
                            .as_ref()
                            .map(fs_model::icon_badge)
                            .unwrap_or("FILE");
                        let thumb = self.thumbnail_for(ui.ctx(), &path, 512);

                        // A dedicated preview canvas keeps every visual type aligned
                        // while preserving the source aspect ratio instead of forcing
                        // thumbnails into a fixed 220x160 rectangle.
                        let canvas_w = ui.available_width().max(120.0);
                        let canvas_h = (canvas_w * 0.68).clamp(150.0, 250.0);
                        let (canvas, _) =
                            ui.allocate_exact_size(Vec2::new(canvas_w, canvas_h), Sense::hover());
                        ui.painter().rect_filled(canvas, 8.0, self.theme.panel_alt);
                        ui.painter().rect_stroke(
                            canvas,
                            8.0,
                            Stroke::new(1.0_f32, self.theme.border),
                            egui::StrokeKind::Middle,
                        );

                        if let Some(tex) = thumb {
                            let source = tex.size_vec2();
                            let bounds = canvas.shrink(12.0).size();
                            let scale = if source.x > 0.0 && source.y > 0.0 {
                                (bounds.x / source.x).min(bounds.y / source.y)
                            } else {
                                1.0
                            };
                            let fitted = Vec2::new(source.x * scale, source.y * scale);
                            let image_rect = Rect::from_center_size(canvas.center(), fitted);
                            ui.put(
                                image_rect,
                                egui::Image::new((tex.id(), fitted)).corner_radius(6.0),
                            );
                        } else {
                            let icon_size = Vec2::new(
                                (canvas.width() * 0.42).clamp(92.0, 132.0),
                                (canvas.height() * 0.55).clamp(80.0, 118.0),
                            );
                            let icon_rect = Rect::from_center_size(canvas.center(), icon_size);
                            if is_dir {
                                paint_folder_icon(
                                    ui.painter(),
                                    icon_rect.shrink(10.0),
                                    self.theme.accent,
                                );
                            } else {
                                paint_typed_file(
                                    ui.painter(),
                                    icon_rect.shrink(10.0),
                                    badge,
                                    self.theme.muted,
                                    self.theme.accent,
                                );
                            }
                        }

                        ui.add_space(12.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(
                                    path.file_name().and_then(|s| s.to_str()).unwrap_or("Item"),
                                )
                                .size(18.0)
                                .strong(),
                            )
                            .wrap(),
                        );
                        ui.add_space(2.0);
                        ui.label(
                            RichText::new(display_type)
                                .size(13.0)
                                .color(self.theme.muted),
                        );
                        ui.add_space(14.0);

                        egui::Frame::new()
                            .fill(self.theme.panel_alt)
                            .corner_radius(7.0)
                            .stroke(Stroke::new(1.0_f32, self.theme.border))
                            .inner_margin(egui::Margin::same(12))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.label(RichText::new("Details").strong());
                                ui.add_space(9.0);

                                preview_property(
                                    ui,
                                    "Location",
                                    path.parent()
                                        .unwrap_or(Path::new("/"))
                                        .to_string_lossy()
                                        .as_ref(),
                                    self.theme.muted,
                                );

                                if let Some(entry) = &selected_entry {
                                    if entry.is_dir {
                                        let stats = self.folder_stats.as_ref().filter(|_| {
                                            self.folder_stats_path.as_ref() == Some(&path)
                                        });
                                        let files = stats
                                            .map(|s| s.file_count.to_string())
                                            .unwrap_or_else(|| "Loading…".into());
                                        let folders = stats
                                            .map(|s| s.folder_count.to_string())
                                            .unwrap_or_else(|| "Loading…".into());
                                        let size = stats
                                            .map(|s| fs_model::format_size(s.apparent_size))
                                            .unwrap_or_else(|| "Loading…".into());
                                        let allocated = stats
                                            .map(|s| fs_model::format_size(s.allocated_size))
                                            .unwrap_or_else(|| "Loading…".into());
                                        let volume = stats
                                            .map(|s| {
                                                if s.total_space == 0 {
                                                    "—".into()
                                                } else {
                                                    format!(
                                                        "{} free of {}",
                                                        fs_model::format_size(s.free_space),
                                                        fs_model::format_size(s.total_space)
                                                    )
                                                }
                                            })
                                            .unwrap_or_else(|| "Loading…".into());
                                        let modified = stats
                                            .map(|s| fs_model::format_modified(s.modified))
                                            .unwrap_or_else(|| "Loading…".into());
                                        let accessed = stats
                                            .map(|s| fs_model::format_modified(s.accessed))
                                            .unwrap_or_else(|| "Loading…".into());
                                        preview_property(ui, "Files", &files, self.theme.muted);
                                        preview_property(ui, "Folders", &folders, self.theme.muted);
                                        preview_property(ui, "Size", &size, self.theme.muted);
                                        preview_property(
                                            ui,
                                            "Space on disk",
                                            &allocated,
                                            self.theme.muted,
                                        );
                                        preview_property(ui, "Volume", &volume, self.theme.muted);
                                        preview_property(
                                            ui,
                                            "Modified",
                                            &modified,
                                            self.theme.muted,
                                        );
                                        preview_property(
                                            ui,
                                            "Accessed",
                                            &accessed,
                                            self.theme.muted,
                                        );
                                    } else {
                                        let size = if entry.metadata_loaded {
                                            fs_model::format_size(entry.size)
                                        } else {
                                            "Loading…".into()
                                        };
                                        let modified = if entry.metadata_loaded {
                                            fs_model::format_modified(entry.modified)
                                        } else {
                                            "Loading…".into()
                                        };
                                        let accessed = if entry.metadata_loaded {
                                            fs_model::format_modified(entry.accessed)
                                        } else {
                                            "Loading…".into()
                                        };
                                        preview_property(ui, "Size", &size, self.theme.muted);
                                        preview_property(
                                            ui,
                                            "Modified",
                                            &modified,
                                            self.theme.muted,
                                        );
                                        preview_property(
                                            ui,
                                            "Accessed",
                                            &accessed,
                                            self.theme.muted,
                                        );
                                    }
                                }
                            });

                        ui.add_space(12.0);
                        ui.label(RichText::new("Quick Actions").strong());
                        ui.add_space(6.0);

                        let quick_open = selected_entry.clone().filter(|entry| {
                            !entry.is_dir
                                && quick_button(
                                    ui,
                                    "↗",
                                    "Open",
                                    self.theme.panel_alt,
                                    self.theme.border,
                                )
                                .clicked()
                        });
                        if let Some(entry) = quick_open {
                            self.open_entry(&entry);
                        }
                        if is_dir
                            && quick_button(
                                ui,
                                "▣",
                                "Open in New Tab",
                                self.theme.panel_alt,
                                self.theme.border,
                            )
                            .clicked()
                        {
                            self.new_tab(path.clone(), true);
                        }
                        if quick_button(
                            ui,
                            ">_",
                            "Open in Terminal",
                            self.theme.panel_alt,
                            self.theme.border,
                        )
                        .clicked()
                        {
                            let p = if is_dir {
                                path.clone()
                            } else {
                                path.parent().unwrap_or(self.current_path()).to_path_buf()
                            };
                            self.open_terminal(&p);
                        }
                        if quick_button(
                            ui,
                            "◆",
                            "Open as Administrator",
                            self.theme.panel_alt,
                            self.theme.border,
                        )
                        .clicked()
                        {
                            let p = if is_dir {
                                path.clone()
                            } else {
                                path.parent().unwrap_or(self.current_path()).to_path_buf()
                            };
                            self.open_admin_terminal(&p);
                        }
                        if fs_model::is_in_trash(&path) {
                            if quick_button(
                                ui,
                                "↶",
                                "Restore",
                                self.theme.panel_alt,
                                self.theme.border,
                            )
                            .clicked()
                            {
                                self.restore_selected_from_trash();
                            }
                        } else if is_dir {
                            let pinned = self.pinned.contains(&path);
                            let label = if pinned { "Unpin Folder" } else { "Pin Folder" };
                            let icon = if pinned { "♥" } else { "♡" };
                            if quick_button(
                                ui,
                                icon,
                                label,
                                self.theme.panel_alt,
                                self.theme.border,
                            )
                            .clicked()
                            {
                                if pinned {
                                    self.unpin_folder(&path);
                                } else {
                                    self.pin_folder(path.clone());
                                }
                            }
                        }
                    } else {
                        ui.vertical_centered(|ui| {
                            ui.add_space(74.0);
                            self.paint_logo(ui, 76.0);
                            ui.add_space(10.0);
                            ui.label(RichText::new("Nothing selected").size(18.0).strong());
                            ui.add_space(2.0);
                            ui.label(
                                RichText::new("Select a file or folder to preview it here.")
                                    .color(self.theme.muted),
                            );
                        });
                    }
                });
        });
    }

    fn preferences_view(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let mut changed = false;
        let mut update_changed = false;
        let opacity_before = self.settings.window_opacity;
        let theme_before = self.settings.theme_preset.clone();
        let sort_before = self.sort;
        let sort_ascending_before = self.sort_ascending;
        let pref_panel = self.theme.panel_alt;
        let pref_border = self.theme.border;
        let pref_muted = self.theme.muted;
        let card = |ui: &mut egui::Ui,
                    title: &str,
                    subtitle: &str,
                    body: &mut dyn FnMut(&mut egui::Ui)| {
            egui::Frame::new()
                .fill(pref_panel)
                .stroke(Stroke::new(1.0_f32, pref_border))
                .corner_radius(9.0)
                .inner_margin(egui::Margin::same(14))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(title).size(16.0).strong());
                    ui.label(RichText::new(subtitle).size(12.5).color(pref_muted));
                    ui.add_space(10.0);
                    body(ui);
                });
        };

        egui::Frame::new()
            .inner_margin(egui::Margin::symmetric(16, 14))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    self.paint_logo(ui, 34.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Preferences").size(21.0).strong());
                        ui.label(RichText::new("Changes apply and save immediately.").color(self.theme.muted));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Close").clicked() { self.show_preferences = false; }
                        ui.add_space(8.0);
                        egui::Frame::new()
                            .fill(self.theme.accent_soft)
                            .corner_radius(7.0)
                            .inner_margin(egui::Margin::symmetric(8, 4))
                            .show(ui, |ui| {
                                ui.label(RichText::new("Saved automatically").size(11.5).color(self.theme.accent_green));
                            });
                    });
                });
                ui.add_space(10.0);

                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    egui::Frame::new()
                        .inner_margin(egui::Margin::symmetric(2, 2))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());

                    card(ui, "Browsing", "Control the default file view and what Hoard displays.", &mut |ui| {
                        changed |= ui.checkbox(&mut self.settings.thumbnails, "Show thumbnails for visual media").changed();
                        changed |= ui.checkbox(&mut self.settings.show_preview, "Show the inspector / preview pane").changed();
                        changed |= ui.checkbox(&mut self.show_hidden, "Show hidden files").changed();
                        ui.add_space(6.0);
                        ui.separator();
                        ui.label(RichText::new("Defaults").size(12.5).strong());
                        egui::Grid::new("preferences-browsing-defaults")
                            .num_columns(2)
                            .spacing(Vec2::new(14.0, 7.0))
                            .show(ui, |ui| {
                                ui.label("View");
                                ui.horizontal(|ui| {
                                    changed |= ui.selectable_value(&mut self.view, ViewMode::Details, "Details").changed();
                                    changed |= ui.selectable_value(&mut self.view, ViewMode::Icons, "Icons").changed();
                                });
                                ui.end_row();

                                ui.label("Sort by");
                                ui.horizontal(|ui| {
                                    changed |= ui.selectable_value(&mut self.sort, SortColumn::Name, "Name").changed();
                                    changed |= ui.selectable_value(&mut self.sort, SortColumn::Type, "Type").changed();
                                    changed |= ui.selectable_value(&mut self.sort, SortColumn::Size, "Size").changed();
                                    changed |= ui.selectable_value(&mut self.sort, SortColumn::Modified, "Modified").changed();
                                    changed |= ui.selectable_value(&mut self.sort, SortColumn::Created, "Created").changed();
                                    changed |= ui.selectable_value(&mut self.sort, SortColumn::Owner, "Owner").changed();
                                    changed |= ui.selectable_value(&mut self.sort, SortColumn::Permissions, "Permissions").changed();
                                });
                                ui.end_row();

                                ui.label("Sort order");
                                ui.horizontal(|ui| {
                                    changed |= ui.selectable_value(&mut self.sort_ascending, true, "Ascending").changed();
                                    changed |= ui.selectable_value(&mut self.sort_ascending, false, "Descending").changed();
                                });
                                ui.end_row();

                                ui.label("Icon size");
                                changed |= ui.add(egui::Slider::new(&mut self.settings.thumbnail_size, 64..=192).show_value(true)).changed();
                                ui.end_row();
                            });
                        ui.add_space(6.0);
                        changed |= ui.checkbox(&mut self.settings.open_folders_in_new_tab, "Open folders in a new tab by default").changed();
                        changed |= ui.checkbox(&mut self.settings.recursive_search_default, "Start new windows with recursive search enabled").changed();
                        ui.horizontal(|ui| {
                            ui.label("Recursive result limit");
                            changed |= ui.add(egui::DragValue::new(&mut self.settings.recursive_search_max_results).range(100..=50_000).speed(100)).changed();
                        });
                    });

                    ui.add_space(10.0);
                    card(ui, "Interface", "Keep the main window compact while preserving the tools you use.", &mut |ui| {
                        changed |= ui.checkbox(&mut self.settings.toolbar_labels, "Show toolbar text labels when space allows").changed();
                        ui.label(RichText::new("Toolbar labels automatically collapse on narrower windows so the location and search fields remain usable.").size(12.0).color(self.theme.muted));
                    });

                    ui.add_space(10.0);
                    card(ui, "Appearance", "Choose a Hoard palette and control how much of the desktop shows through structural surfaces.", &mut |ui| {
                        ui.label(RichText::new("Theme").strong());
                        ui.add_space(4.0);

                        egui::ComboBox::from_id_salt("hoard-theme-preset")
                            .selected_text(match self.settings.theme_preset.as_str() {
                                "ocean-teal" => "Ocean Teal",
                                "amber-graphite" => "Amber Graphite",
                                "cobalt-night" => "Cobalt Night",
                                "ember-stone" => "Ember Stone",
                                "forest-carbon" => "Forest Carbon",
                                "plum-rose" => "Plum Rose",
                                "ivory-ink" => "Ivory Ink",
                                "solar-paper" => "Solar Paper",
                                "nord-aurora" => "Nord Aurora",
                                _ => "Obsidian Violet",
                            })
                            .width(220.0)
                            .show_ui(ui, |ui| {
                                for (key, label) in [
                                    ("obsidian-violet", "Obsidian Violet"),
                                    ("ocean-teal", "Ocean Teal"),
                                    ("amber-graphite", "Amber Graphite"),
                                    ("cobalt-night", "Cobalt Night"),
                                    ("ember-stone", "Ember Stone"),
                                    ("forest-carbon", "Forest Carbon"),
                                    ("plum-rose", "Plum Rose"),
                                    ("ivory-ink", "Ivory Ink"),
                                    ("solar-paper", "Solar Paper"),
                                    ("nord-aurora", "Nord Aurora"),
                                ] {
                                    changed |= ui
                                        .selectable_value(
                                            &mut self.settings.theme_preset,
                                            key.to_string(),
                                            label,
                                        )
                                        .changed();
                                }
                            });

                        ui.label(
                            RichText::new(
                                "Hoard uses the same ten bundled palettes as Tiamat while preserving Hoard's file-manager layout and metrics."
                            )
                            .size(12.0)
                            .color(self.theme.muted),
                        );

                        ui.horizontal(|ui| {
                            if ui.small_button("Reload Theme").clicked() {
                                self.reload_theme_with_opacity(ctx);
                                self.egui_ctx.request_repaint();
                                self.status = "Theme reloaded".into();
                            }
                            ui.label(
                                RichText::new("Use only if an external theme edit or rendering issue needs a manual refresh.")
                                    .size(11.5)
                                    .color(self.theme.muted),
                            );
                        });

                        ui.add_space(10.0);

                        ui.horizontal(|ui| {
                            ui.label("Window opacity");
                            ui.add_space(8.0);
                            let opacity = ui.add(
                                egui::Slider::new(&mut self.settings.window_opacity, 0.55..=1.0)
                                    .show_value(false),
                            );
                            changed |= opacity.changed();
                            ui.label(RichText::new(format!("{:.0}%", self.settings.window_opacity * 100.0)).strong());
                        });
                        ui.add_space(5.0);
                        ui.label(RichText::new("Only Hoard's structural surfaces become translucent; text, icons, borders, and selection indicators remain fully legible.").size(12.0).color(self.theme.muted));
                        ui.horizontal(|ui| {
                            if ui.small_button("85%").clicked() { self.settings.window_opacity = 0.85; changed = true; }
                            if ui.small_button("90%").clicked() { self.settings.window_opacity = 0.90; changed = true; }
                            if ui.small_button("95%").clicked() { self.settings.window_opacity = 0.95; changed = true; }
                            if ui.small_button("Opaque").clicked() { self.settings.window_opacity = 1.0; changed = true; }
                        });
                    });

                    ui.add_space(10.0);
                    card(ui, "Behavior", "Session, deletion, and terminal integration.", &mut |ui| {
                        changed |= ui.checkbox(&mut self.settings.remember_tabs, "Restore open tabs when Hoard starts").changed();
                        changed |= ui.checkbox(&mut self.settings.confirm_permanent_delete, "Confirm permanent deletion").changed();
                        ui.add_space(8.0);
                        egui::Grid::new("preferences-behavior-fields")
                            .num_columns(2)
                            .spacing(Vec2::new(14.0, 7.0))
                            .show(ui, |ui| {
                                ui.label("Terminal");
                                changed |= ui.add_sized([300.0, 28.0], Self::singleline_text_edit(&mut self.settings.terminal)).changed();
                                ui.end_row();
                            });
                        ui.add_space(5.0);
                        ui.label(RichText::new("Middle-click folders for a background tab; Shift + double-click opens a new window.").size(12.0).color(self.theme.muted));
                        ui.label(RichText::new("Drag items onto folders, tabs, or sidebar locations. Hold Ctrl while dropping to copy.").size(12.0).color(self.theme.muted));
                    });

                    ui.add_space(10.0);
                    card(ui, "Keyboard Reference", "Core keyboard workflow. Informational; no configuration required.", &mut |ui| {
                        for line in [
                            "Ctrl+L  Focus location", "Ctrl+Z  Undo safe operation", "Ctrl+Shift+Z  Redo", "F2  Rename",
                            "Delete  Move to Trash", "Shift+Delete  Delete permanently", "Ctrl+A  Select all",
                            "Ctrl+Shift+T  Reopen closed tab", "Esc  Clear or close transient UI",
                        ] { ui.label(RichText::new(line).size(12.5).color(self.theme.muted)); }
                    });

                    ui.add_space(10.0);
                    card(ui, "Updates", "Public release updates are verified before installation and can roll back on failure.", &mut |ui| {
                        update_changed |= ui.checkbox(&mut self.update_config.enabled, "Enable update checks").changed();
                        egui::Grid::new("preferences-update-fields")
                            .num_columns(2)
                            .spacing(Vec2::new(14.0, 7.0))
                            .show(ui, |ui| {
                                ui.label("Channel");
                                update_changed |= ui.selectable_value(&mut self.update_config.channel, "stable".into(), "Stable").changed();
                                ui.end_row();

                                ui.label("Check interval");
                                update_changed |= ui.add(egui::DragValue::new(&mut self.update_config.check_interval_hours).range(1..=168).suffix(" hours")).changed();
                                ui.end_row();
                            });
                        ui.add_space(5.0);
                        ui.label(RichText::new(format!("Source: GitHub Releases • {}", self.update_config.repository)).size(12.0).color(self.theme.muted));
                        ui.horizontal(|ui| {
                            if ui.add_enabled(self.update_rx.is_none(), egui::Button::new("Check for Updates Now")).clicked() {
                                if let Err(e) = config::save_update_config(&self.update_config) {
                                    self.status = format!("Could not save update preferences: {e}");
                                } else {
                                    self.check_updates();
                                }
                            }
                            if ui.button("Open Update Config").clicked() {
                                let _ = config::save_update_config(&self.update_config);
                                let path = config::update_config_path();
                                self.open_path_external(&path, "update configuration");
                            }
                        });
                    });

                    ui.add_space(10.0);
                    card(ui, "About & Diagnostics", "Release identity and privacy-conscious support information.", &mut |ui| {
                        ui.label(RichText::new(format!("Hoard {}", env!("CARGO_PKG_VERSION"))).strong());
                        ui.label(RichText::new(format!("Build: {}", hoard_core::identity::build_identity().candidate_revision.as_str())).size(12.0).color(self.theme.muted));
                        ui.label(RichText::new("Native Wayland • Smithay Client Toolkit • egui-wgpu • Vulkan").size(12.0).color(self.theme.accent_green));
                        ui.add_space(6.0);
                        for (label, path) in [
                            ("Config", config::config_dir()),
                            ("Cache", config::cache_dir()),
                            ("State", config::state_dir()),
                        ] {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format!("{label}:")).size(12.0).strong());
                                ui.add(egui::Label::new(RichText::new(path.display().to_string()).size(12.0).color(self.theme.muted)).wrap());
                            });
                        }
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button("Copy Diagnostics").clicked() {
                                self.write_system_text_clipboard(&diagnostics::diagnostics_text());
                            }
                            if ui.button("Open Config Folder").clicked() {
                                self.open_path_external(&config::config_dir(), "config folder");
                            }
                            if ui.button("Open Cache Folder").clicked() {
                                self.open_path_external(&config::cache_dir(), "cache folder");
                            }
                        });
                        ui.label(RichText::new("Diagnostics intentionally omit browsing history and private filenames.").size(11.5).color(self.theme.muted));
                    });

                    ui.add_space(10.0);
                    card(ui, "Runtime", "Current desktop backend and integration status.", &mut |ui| {
                        ui.label(RichText::new("Native Wayland + Vulkan").strong().color(self.theme.accent_green));
                        ui.label(RichText::new("Hoard runs directly on Wayland with SCTK and a Vulkan-only wgpu renderer. eframe, winit, EGL, GLX and XWayland are not part of the window/render path.").size(12.0).color(self.theme.muted));
                        ui.label(RichText::new("File drag/drop uses the native Wayland data-device protocol while preserving Hoard's existing drop targets and operation safety.").size(12.0).color(self.theme.muted));
                    });

                    ui.add_space(10.0);
                    card(ui, "Advanced", "Cache management, theme reload, and safe reset controls.", &mut |ui| {
                        changed |= ui.checkbox(&mut self.settings.directory_cache, "Enable persistent directory cache").changed();
                        changed |= ui.checkbox(&mut self.settings.thumbnail_disk_cache, "Enable persistent thumbnail cache").changed();
                        ui.horizontal(|ui| {
                            ui.label("Thumbnail cache limit");
                            changed |= ui.add(egui::Slider::new(&mut self.settings.thumbnail_cache_limit_mb, 128..=4096).suffix(" MB")).changed();
                        });
                        ui.horizontal(|ui| {
                            ui.label("Thumbnail max age");
                            changed |= ui.add(egui::Slider::new(&mut self.settings.thumbnail_cache_max_age_days, 1..=180).suffix(" days")).changed();
                        });
                        changed |= ui.checkbox(&mut self.settings.recursive_search_remote, "Allow recursive search on remote filesystems").changed();
                        ui.add_space(4.0);
                        ui.label(RichText::new("Cache status").size(12.5).strong());
                        ui.label(RichText::new(format!("Directory {}  •  Thumbnails {}  •  Hits/Misses {}/{}", fs_model::format_size(cache::directory_cache_bytes()), fs_model::format_size(cache::thumbnail_cache_bytes()), self.cache_hits, self.cache_misses)).size(12.0).color(self.theme.muted));
                        ui.horizontal(|ui| {
                            if ui.button("Clear Directory Cache").clicked() {
                                match cache::clear_directory_cache() { Ok(_) => self.status = "Directory cache cleared".into(), Err(e) => self.status = format!("Could not clear directory cache: {e}") }
                            }
                            if ui.button("Clear Thumbnail Cache").clicked() {
                                match cache::clear_thumbnail_cache() { Ok(_) => { self.thumbnails.clear(); self.thumbnail_failed.clear(); self.status = "Thumbnail cache cleared".into(); }, Err(e) => self.status = format!("Could not clear thumbnail cache: {e}") }
                            }
                        });
                        ui.separator();
                        ui.label(RichText::new("Theme file").strong());
                        ui.add(egui::Label::new(RichText::new(config::theme_path().display().to_string()).size(12.0).color(self.theme.muted)).wrap());
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button("Open Theme File").clicked() {
                                let p = config::theme_path().to_string_lossy().to_string();
                                let _ = Self::spawn_quiet("xdg-open", &[&p]);
                            }
                            if ui.button(RichText::new("Reset Settings to Defaults").color(self.theme.danger)).clicked() {
                                match config::reset_settings() {
                                    Ok(defaults) => {
                                        self.settings = defaults;
                                        self.show_hidden = self.settings.show_hidden;
                                        self.recursive_search = self.settings.recursive_search_default;
                                        self.reload_theme_with_opacity(ctx);
                                        self.dirty = true;
                                        self.status = "Settings reset to defaults".into();
                                    }
                                    Err(e) => self.status = format!("Could not reset settings: {e}"),
                                }
                            }
                        });
                        ui.label(RichText::new("Settings are written immediately to the user configuration. Reset affects preferences only; files, pins, and file-operation history are not deleted.").size(11.5).color(self.theme.muted));
                    });
                    ui.add_space(12.0);
                        });
                });
            });
        });

        if update_changed {
            self.update_config.check_interval_hours =
                self.update_config.check_interval_hours.clamp(1, 168);
            if let Err(e) = config::save_update_config(&self.update_config) {
                self.status = format!("Could not save update preferences: {e}");
            } else {
                self.status = "Update preferences saved".into();
            }
        }

        if changed {
            if self.sort != sort_before || self.sort_ascending != sort_ascending_before {
                fs_model::sort_entries(&mut self.entries, self.sort, self.sort_ascending);
                self.rebuild_entry_index();
            }
            self.settings.window_opacity = self.settings.window_opacity.clamp(0.55, 1.0);
            if self.settings.theme_preset != theme_before
                || (self.settings.window_opacity - opacity_before).abs() > f32::EPSILON
            {
                self.reload_theme_with_opacity(ctx);
            }
            self.settings.show_hidden = self.show_hidden;
            self.save_preferences();
            self.thumbnails.clear();
            self.dirty = true;
            self.persist_session();
            self.egui_ctx.request_repaint();
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        if let Some(pending) = self.pending_collision.clone() {
            let mut open = true;
            egui::Window::new("File name conflict")
                .open(&mut open)
                .resizable(false)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(format!("{} item(s) already exist in the destination.", pending.conflicts));
                    ui.label(RichText::new("Choose one rule for this queued operation. Hoard will never overwrite implicitly.").color(self.theme.muted));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Keep Both").clicked() {
                            self.pending_collision = None;
                            self.enqueue_file_operation(QueuedFileOperation { paths: pending.paths.clone(), dest: pending.dest.clone(), mode: pending.mode, collision: fs_model::CollisionPolicy::KeepBoth });
                        }
                        if ui.button("Skip Existing").clicked() {
                            self.pending_collision = None;
                            self.enqueue_file_operation(QueuedFileOperation { paths: pending.paths.clone(), dest: pending.dest.clone(), mode: pending.mode, collision: fs_model::CollisionPolicy::Skip });
                        }
                        if ui.button(RichText::new("Replace Existing").color(self.theme.danger)).clicked() {
                            self.pending_collision = None;
                            self.enqueue_file_operation(QueuedFileOperation { paths: pending.paths.clone(), dest: pending.dest.clone(), mode: pending.mode, collision: fs_model::CollisionPolicy::Replace });
                        }
                        if ui.button("Cancel").clicked() { self.pending_collision = None; self.status = "Operation cancelled".into(); }
                    });
                });
            if !open {
                self.pending_collision = None;
            }
        }

        if self.rename_target.is_some() {
            let target = self.rename_target.clone().unwrap();
            let mut open = true;
            egui::Window::new("Rename")
                .open(&mut open)
                .resizable(false)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Rename {}",
                        target
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("item")
                    ));
                    let r = ui.add(Self::singleline_text_edit(&mut self.rename_text));
                    if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
                        || ui.button("Rename").clicked()
                    {
                        let name = self.rename_text.clone();
                        self.rename_target = None;
                        self.enqueue_operation(QueuedOperation::Rename {
                            src: target.clone(),
                            new_name: name,
                        });
                    }
                });
            if !open {
                self.rename_target = None;
            }
        }

        if self.show_new_folder {
            let mut open = self.show_new_folder;
            egui::Window::new("New Folder")
                .open(&mut open)
                .resizable(false)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(
                        RichText::new("Folder name")
                            .size(12.0)
                            .color(self.theme.muted),
                    );
                    let name = ui.add_sized(
                        [360.0, 30.0],
                        Self::singleline_text_edit(&mut self.new_folder_name),
                    );
                    name.request_focus();
                    let submit = ui.input(|i| i.key_pressed(egui::Key::Enter))
                        || ui.button("Create Folder").clicked();
                    if submit {
                        let trimmed = self.new_folder_name.trim();
                        if trimmed.is_empty() {
                            self.status = "Folder name cannot be empty".into();
                        } else {
                            let parent = self.current_path().to_path_buf();
                            let name = trimmed.to_string();
                            self.show_new_folder = false;
                            self.enqueue_operation(QueuedOperation::CreateFolder { parent, name });
                        }
                    }
                });
            if !open {
                self.show_new_folder = false;
            }
        }

        if self.show_new_file {
            let mut open = self.show_new_file;
            egui::Window::new("New File")
                .open(&mut open)
                .resizable(false)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(
                        RichText::new("File name")
                            .size(12.0)
                            .color(self.theme.muted),
                    );
                    let name = ui.add_sized(
                        [360.0, 30.0],
                        Self::singleline_text_edit(&mut self.new_file_name),
                    );
                    name.request_focus();
                    let submit = ui.input(|i| i.key_pressed(egui::Key::Enter))
                        || ui.button("Create File").clicked();
                    if submit {
                        let trimmed = self.new_file_name.trim();
                        if trimmed.is_empty() {
                            self.status = "File name cannot be empty".into();
                        } else {
                            let parent = self.current_path().to_path_buf();
                            let name = trimmed.to_string();
                            self.show_new_file = false;
                            self.enqueue_operation(QueuedOperation::CreateFile { parent, name });
                        }
                    }
                });
            if !open {
                self.show_new_file = false;
            }
        }

        if !self.delete_targets.is_empty() {
            let targets = self.delete_targets.clone();
            let count = targets.len();
            if !self.settings.confirm_permanent_delete {
                self.enqueue_operation(QueuedOperation::DeletePermanent(targets));
                self.delete_targets.clear();
                self.selected = None;
                self.selected_paths.clear();
            } else {
                let mut open = true;
                egui::Window::new("Delete Permanently?")
                    .open(&mut open)
                    .resizable(false)
                    .collapsible(false)
                    .show(ctx, |ui| {
                        ui.label(format!(
                            "Permanently delete {count} item(s)? This cannot be undone."
                        ));
                        if count == 1 {
                            ui.label(
                                RichText::new(targets[0].to_string_lossy()).color(self.theme.muted),
                            );
                        } else {
                            for path in targets.iter().take(5) {
                                ui.label(
                                    RichText::new(path.to_string_lossy())
                                        .size(11.5)
                                        .color(self.theme.muted),
                                );
                            }
                            if count > 5 {
                                ui.label(
                                    RichText::new(format!("…and {} more", count - 5))
                                        .color(self.theme.muted),
                                );
                            }
                        }
                        ui.horizontal(|ui| {
                            if ui.button("Cancel").clicked() {
                                self.delete_targets.clear();
                            }
                            if ui
                                .button(
                                    RichText::new("Delete Permanently").color(self.theme.danger),
                                )
                                .clicked()
                            {
                                self.enqueue_operation(QueuedOperation::DeletePermanent(
                                    targets.clone(),
                                ));
                                self.delete_targets.clear();
                                self.selected = None;
                                self.selected_paths.clear();
                            }
                        });
                    });
                if !open {
                    self.delete_targets.clear();
                }
            }
        }

        if self.empty_trash_confirm {
            let mut open = true;
            egui::Window::new("Empty Trash?")
                .open(&mut open)
                .resizable(false)
                .collapsible(false)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Permanently delete {} item(s) from Trash?",
                        self.trash_count
                    ));
                    ui.label(RichText::new("This cannot be undone.").color(self.theme.danger));
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.empty_trash_confirm = false;
                        }
                        if ui
                            .button(RichText::new("Empty Trash").color(self.theme.danger))
                            .clicked()
                        {
                            self.enqueue_operation(QueuedOperation::EmptyTrash);
                            self.empty_trash_confirm = false;
                            self.selected = None;
                            self.selected_paths.clear();
                        }
                    });
                });
            if !open {
                self.empty_trash_confirm = false;
            }
        }
    }

    fn bottom_bar(&mut self, root: &mut egui::Ui) {
        let ctx = root.ctx().clone();
        let t = self.theme.clone();
        egui::Panel::bottom("status")
            .exact_size(46.0)
            .frame(
                egui::Frame::new()
                    .fill(Self::darken_surface(t.sidebar, 0.72))
                    .stroke(Stroke::new(1.0_f32, t.border))
                    .inner_margin(egui::Margin::symmetric(10, 6)),
            )
            .show(root, |ui| {
                ui.horizontal(|ui| {
                    ui.add_space(t.metrics.sidebar_width + 6.0);
                    let selected = self.selected_paths.len();
                    ui.label(RichText::new(format!("{} selected", selected)).color(t.muted));
                    ui.separator();
                    ui.label(RichText::new(format!("{} items", self.entries.len())).color(t.muted));
                    if self.loading || self.recursive_searching {
                        ui.separator();
                        ui.spinner();
                        ui.label(
                            RichText::new(if self.recursive_searching {
                                "Searching…"
                            } else {
                                "Loading…"
                            })
                            .color(t.accent),
                        );
                    }
                    if self.in_trash() {
                        ui.separator();
                        ui.label(
                            RichText::new(format!("Trash: {} item(s)", self.trash_count))
                                .color(t.accent),
                        );
                    }
                    if let Some((done, total, name, bytes_done, total_bytes)) =
                        self.file_op_progress.clone()
                    {
                        ui.separator();
                        let frac = if let Some(total_bytes) = total_bytes {
                            if total_bytes == 0 {
                                0.0
                            } else {
                                bytes_done as f32 / total_bytes as f32
                            }
                        } else if total == 0 {
                            0.0
                        } else {
                            done as f32 / total as f32
                        };
                        let progress_text = if let Some(total_bytes) = total_bytes {
                            format!(
                                "{} / {}",
                                fs_model::format_size(bytes_done),
                                fs_model::format_size(total_bytes)
                            )
                        } else {
                            format!("{done}/{total}")
                        };
                        ui.add(
                            egui::ProgressBar::new(frac.clamp(0.0, 1.0))
                                .desired_width(145.0)
                                .text(progress_text),
                        );
                        if ctx.content_rect().width() > 1220.0 {
                            ui.label(RichText::new(shorten(&name, 24)).color(t.muted));
                        }
                        if !self.file_op_queue.is_empty() {
                            ui.label(
                                RichText::new(format!("+{} queued", self.file_op_queue.len()))
                                    .color(t.muted),
                            );
                        }
                        if ui
                            .small_button("Cancel")
                            .on_hover_text("Cancel after the current item completes")
                            .clicked()
                        {
                            self.cancel_file_operation();
                        }
                    }
                    if let Some(payload) = &self.drag_payload {
                        let copy = ctx.input(|i| i.modifiers.ctrl);
                        ui.separator();
                        ui.label(
                            RichText::new(format!(
                                "{} {} item(s) — release over a highlighted folder",
                                if copy { "Copy" } else { "Move" },
                                payload.paths.len()
                            ))
                            .color(t.accent_green),
                        );
                    } else if !self.status.is_empty() {
                        ui.separator();
                        ui.label(RichText::new(&self.status).color(t.accent_green));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let status_space = if ctx.content_rect().width() >= 1320.0 {
                            self.current_space
                        } else {
                            None
                        };
                        if let Some((free, total)) = status_space {
                            let frac = if total == 0 {
                                0.0
                            } else {
                                1.0 - (free as f32 / total as f32)
                            };
                            ui.add(egui::ProgressBar::new(frac).desired_width(210.0).text(
                                format!(
                                    "{} free ({})",
                                    fs_model::format_size(free),
                                    fs_model::format_size(total)
                                ),
                            ));
                        }
                        ui.separator();
                        ui.label(RichText::new("Icon size").color(t.muted));
                        let slider_resp = ui
                            .add_sized(
                                [108.0, 18.0],
                                egui::Slider::new(&mut self.settings.thumbnail_size, 64..=192)
                                    .show_value(false)
                                    .step_by(8.0)
                                    .clamping(egui::SliderClamping::Always),
                            )
                            .on_hover_text("Adjust icon thumbnail size");
                        ui.label(
                            RichText::new(format!("{}", self.settings.thumbnail_size))
                                .color(t.muted),
                        );
                        if slider_resp.changed() {
                            self.persist_browsing_settings();
                            self.egui_ctx.request_repaint();
                        }
                    });
                });
            });
    }

    fn paint_logo(&self, ui: &mut egui::Ui, size: f32) {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
        let c = rect.center();
        let p = ui.painter();
        let u = size / 42.0;

        // Angular dragon/vault mark: no concentric rings, intentionally faceted.
        let head = vec![
            Pos2::new(c.x, c.y - 17.0 * u),
            Pos2::new(c.x + 12.0 * u, c.y - 8.0 * u),
            Pos2::new(c.x + 9.0 * u, c.y + 8.0 * u),
            Pos2::new(c.x, c.y + 17.0 * u),
            Pos2::new(c.x - 9.0 * u, c.y + 8.0 * u),
            Pos2::new(c.x - 12.0 * u, c.y - 8.0 * u),
        ];
        p.add(egui::Shape::convex_polygon(
            head,
            self.theme.accent_soft,
            Stroke::new(2.2_f32 * u, self.theme.accent),
        ));

        // Horns / crown.
        p.line_segment(
            [
                Pos2::new(c.x - 9.0 * u, c.y - 8.0 * u),
                Pos2::new(c.x - 16.0 * u, c.y - 16.0 * u),
            ],
            Stroke::new(2.6_f32 * u, self.theme.accent),
        );
        p.line_segment(
            [
                Pos2::new(c.x - 16.0 * u, c.y - 16.0 * u),
                Pos2::new(c.x - 13.0 * u, c.y - 5.0 * u),
            ],
            Stroke::new(2.2_f32 * u, self.theme.accent),
        );
        p.line_segment(
            [
                Pos2::new(c.x + 9.0 * u, c.y - 8.0 * u),
                Pos2::new(c.x + 16.0 * u, c.y - 16.0 * u),
            ],
            Stroke::new(2.6_f32 * u, self.theme.accent),
        );
        p.line_segment(
            [
                Pos2::new(c.x + 16.0 * u, c.y - 16.0 * u),
                Pos2::new(c.x + 13.0 * u, c.y - 5.0 * u),
            ],
            Stroke::new(2.2_f32 * u, self.theme.accent),
        );

        // Brow / muzzle geometry.
        p.line_segment(
            [
                Pos2::new(c.x - 8.0 * u, c.y - 4.0 * u),
                Pos2::new(c.x - 2.0 * u, c.y - 1.0 * u),
            ],
            Stroke::new(2.0_f32 * u, self.theme.accent_green),
        );
        p.line_segment(
            [
                Pos2::new(c.x + 8.0 * u, c.y - 4.0 * u),
                Pos2::new(c.x + 2.0 * u, c.y - 1.0 * u),
            ],
            Stroke::new(2.0_f32 * u, self.theme.accent_green),
        );
        p.line_segment(
            [Pos2::new(c.x, c.y - 1.0 * u), Pos2::new(c.x, c.y + 9.0 * u)],
            Stroke::new(1.8_f32 * u, self.theme.accent),
        );
        p.line_segment(
            [
                Pos2::new(c.x - 5.0 * u, c.y + 7.0 * u),
                Pos2::new(c.x, c.y + 11.0 * u),
            ],
            Stroke::new(1.8_f32 * u, self.theme.accent),
        );
        p.line_segment(
            [
                Pos2::new(c.x + 5.0 * u, c.y + 7.0 * u),
                Pos2::new(c.x, c.y + 11.0 * u),
            ],
            Stroke::new(1.8_f32 * u, self.theme.accent),
        );

        // Small triangular eyes.
        let left_eye = vec![
            Pos2::new(c.x - 7.0 * u, c.y - 3.0 * u),
            Pos2::new(c.x - 3.0 * u, c.y - 2.0 * u),
            Pos2::new(c.x - 5.0 * u, c.y + 0.5 * u),
        ];
        let right_eye = vec![
            Pos2::new(c.x + 7.0 * u, c.y - 3.0 * u),
            Pos2::new(c.x + 3.0 * u, c.y - 2.0 * u),
            Pos2::new(c.x + 5.0 * u, c.y + 0.5 * u),
        ];
        p.add(egui::Shape::convex_polygon(
            left_eye,
            self.theme.accent_green,
            Stroke::NONE,
        ));
        p.add(egui::Shape::convex_polygon(
            right_eye,
            self.theme.accent_green,
            Stroke::NONE,
        ));
    }
}

impl HoardApp {
    pub fn take_platform_commands(&mut self) -> Vec<PlatformCommand> {
        self.platform_commands.drain(..).collect()
    }

    pub fn runtime_smoke_ready(&self) -> bool {
        !self.loading
            && self.directory_rx.is_none()
            && !self.recursive_searching
            && !self.file_op_busy
            && self.current_path().is_dir()
            && !self.status.starts_with("Could not load")
    }

    pub fn runtime_smoke_summary(&self) -> (PathBuf, usize) {
        (self.current_path().to_path_buf(), self.entries.len())
    }

    pub fn update_ui(&mut self, root: &mut egui::Ui) {
        let ctx = root.ctx().clone();
        self.hot_reload_theme(&ctx);
        self.poll_trash_count();
        self.watch_current_directory();
        self.keyboard(&ctx);
        if self.dirty {
            if self.recursive_search && !self.search.trim().is_empty() {
                self.dirty = false;
                self.start_recursive_search();
            } else {
                self.start_refresh();
            }
        }
        self.poll_directory();
        self.poll_recursive_search();
        self.poll_space();
        self.poll_folder_stats();
        self.poll_metadata();
        self.poll_file_operations();
        self.poll_thumbnails(&ctx);
        let update_msg = self.update_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(msg) = update_msg {
            self.status = msg;
            self.update_rx = None;
        }
        let open_with_msg = self.open_with_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(msg) = open_with_msg {
            self.status = msg;
            self.open_with_rx = None;
        }

        self.top_bar(root);
        self.toolbar(root);
        self.bottom_bar(root);
        self.sidebar(root);
        self.central(root);
        self.dialogs(&ctx);

        if ctx.input(|i| i.pointer.any_released()) {
            self.drag_payload = None;
            self.platform_commands
                .push_back(PlatformCommand::DisarmFileDrag);
        }
        if self.dirty {
            ctx.request_repaint();
        }

        let screen = ctx.content_rect();
        ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            egui::Id::new("outer-border"),
        ))
        .rect_stroke(
            screen.shrink(1.0),
            9.0,
            Stroke::new(1.0_f32, self.theme.accent),
            egui::StrokeKind::Middle,
        );
    }
}

fn section_label(ui: &mut egui::Ui, text: &str, color: Color32) {
    ui.label(RichText::new(text).strong().color(color));
}
fn preview_property(ui: &mut egui::Ui, key: &str, value: &str, muted: Color32) {
    ui.label(RichText::new(key).size(12.0).color(muted));
    ui.add(egui::Label::new(RichText::new(value).size(13.5)).wrap());
    ui.add_space(8.0);
}
fn quick_button(
    ui: &mut egui::Ui,
    icon: &str,
    label: &str,
    fill: Color32,
    border: Color32,
) -> egui::Response {
    ui.add_sized(
        [ui.available_width(), 36.0],
        egui::Button::new(format!("{}    {}", icon, label))
            .fill(fill)
            .stroke(Stroke::new(1.0_f32, border)),
    )
}

fn paint_folder_icon(p: &egui::Painter, rect: Rect, color: Color32) {
    let dark = Color32::from_rgb(
        color.r().saturating_sub(35),
        color.g().saturating_sub(30),
        color.b().saturating_sub(20),
    );
    let body_w = (rect.width() * 0.78).max(20.0);
    let body_h = (rect.height() * 0.50).max(14.0);
    let body = Rect::from_center_size(
        Pos2::new(rect.center().x, rect.center().y + rect.height() * 0.08),
        Vec2::new(body_w, body_h),
    );
    let tab_w = body_w * 0.42;
    let tab_h = body_h * 0.34;
    let tab = Rect::from_min_size(
        Pos2::new(body.left() + body_w * 0.08, body.top() - tab_h * 0.72),
        Vec2::new(tab_w, tab_h),
    );
    p.rect_filled(tab, 3.0, dark);
    p.rect_filled(body, 5.0, color);
    p.rect_stroke(
        body,
        5.0,
        Stroke::new(1.0_f32, dark),
        egui::StrokeKind::Middle,
    );
}
fn paint_typed_file(
    p: &egui::Painter,
    rect: Rect,
    badge: &str,
    file_color: Color32,
    accent: Color32,
) {
    let w = rect.width().max(18.0);
    let h = rect.height().max(22.0);
    let page_w = (w * 0.72).min(54.0);
    let page_h = (h * 0.82).min(68.0);
    let page = Rect::from_center_size(rect.center(), Vec2::new(page_w, page_h));
    let fold = (page_w * 0.23).clamp(4.0, 12.0);

    p.rect_filled(page, 4.0, file_color);
    p.rect_stroke(
        page,
        4.0,
        Stroke::new(1.0_f32, accent),
        egui::StrokeKind::Middle,
    );
    p.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(page.right() - fold, page.top()),
            Pos2::new(page.right(), page.top()),
            Pos2::new(page.right(), page.top() + fold),
        ],
        accent,
        Stroke::NONE,
    ));

    let font = if page_w < 28.0 {
        7.0
    } else if badge.len() > 3 {
        9.0
    } else {
        11.0
    };
    p.text(
        Pos2::new(page.center().x, page.center().y + page_h * 0.08),
        Align2::CENTER_CENTER,
        badge,
        FontId::proportional(font),
        Color32::WHITE,
    );
}
fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!(
            "{}…",
            s.chars().take(max.saturating_sub(1)).collect::<String>()
        )
    }
}
fn shell_quote(path: &Path) -> String {
    let s = path.to_string_lossy();
    format!("'{}'", s.replace('\'', "'\\''"))
}
