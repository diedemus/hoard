use crate::{
    config::{self, AppSettings},
    fs_model::{self, FileEntry, SortColumn},
    theme::Theme,
    thumbnail,
    updater,
};
use eframe::egui::{self, Align, Align2, Color32, FontId, Layout, Pos2, Rect, RichText, Sense, Stroke, TextureHandle, Vec2};
use std::{
    collections::{HashMap, HashSet},
    io::Write,
    os::fd::AsFd,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, TryRecvError},
    time::{Duration, Instant, SystemTime},
};
use url::Url;

#[derive(Clone)]
struct Tab {
    path: PathBuf,
    back: Vec<PathBuf>,
    forward: Vec<PathBuf>,
    search: String,
    selected: Option<PathBuf>,
    view: ViewMode,
}

impl Tab {
    fn new(path: PathBuf, view: ViewMode) -> Self {
        Self { path, back: Vec::new(), forward: Vec::new(), search: String::new(), selected: None, view }
    }
    fn label(&self) -> String {
        if self.path == dirs::home_dir().unwrap_or_default() { return "Home".into(); }
        if self.path == fs_model::trash_files_dir() { return "Trash".into(); }
        self.path.file_name().and_then(|s| s.to_str()).unwrap_or("/").to_string()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ViewMode { Details, Icons }

#[derive(Clone, Copy)]
enum ToolIcon { Back, Forward, Up, Refresh, Home, Details, Icons, Cut, Copy, Paste }

#[derive(Clone, Copy)]
enum ClipboardMode { Copy, Move }

#[derive(Clone)]
struct ClipboardItem {
    paths: Vec<PathBuf>,
    mode: ClipboardMode,
}

#[derive(Clone, Debug)]
struct DragPayload {
    paths: Vec<PathBuf>,
}

enum FileOpMessage {
    Progress { done: usize, total: usize, name: String },
    Done { succeeded: usize, failed: usize, moved: bool },
}

enum MetadataMessage {
    Batch(PathBuf, Vec<fs_model::EntryMetadata>),
    Done(PathBuf),
}

struct DirectoryLoadResult {
    path: PathBuf,
    entries: Vec<FileEntry>,
    mounts: Vec<(String, PathBuf)>,
    error: Option<String>,
}

pub struct HoardApp {
    theme: Theme,
    egui_ctx: egui::Context,
    theme_mtime: Option<SystemTime>,
    theme_check_rx: Receiver<Option<SystemTime>>,
    settings: AppSettings,
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
    space_rx: Option<Receiver<(PathBuf, Option<(u64, u64)>)>>,
    current_space: Option<(u64, u64)>,
    trash_count: usize,
    trash_count_rx: Receiver<usize>,
    metadata_rx: Option<Receiver<MetadataMessage>>,
    metadata_loading: bool,
    file_op_rx: Option<Receiver<FileOpMessage>>,
    file_op_busy: bool,
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
    delete_target: Option<PathBuf>,
    empty_trash_confirm: bool,
}

impl HoardApp {
    pub fn new(cc: &eframe::CreationContext<'_>, start: Option<PathBuf>) -> Self {
        let mut settings = config::load_settings();
        settings.window_opacity = settings.window_opacity.clamp(0.55, 1.0);
        let theme = Theme::load(&config::theme_path()).with_surface_opacity(settings.window_opacity);
        theme.apply_visuals(&cc.egui_ctx);

        fs_model::ensure_trash_dirs();
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
        let view = if settings.default_view.eq_ignore_ascii_case("icons") { ViewMode::Icons } else { ViewMode::Details };
        let sort = match settings.sort_column.to_ascii_lowercase().as_str() {
            "type" => SortColumn::Type,
            "size" => SortColumn::Size,
            "modified" => SortColumn::Modified,
            _ => SortColumn::Name,
        };
        let tabs = if let Some(start) = start {
            vec![Tab::new(start, view)]
        } else if settings.remember_tabs {
            let session = config::load_session();
            let mut restored: Vec<Tab> = session.tabs.into_iter()
                .map(PathBuf::from)
                .map(|p| Tab::new(p, view))
                .take(24)
                .collect();
            if restored.is_empty() { restored.push(Tab::new(home.clone(), view)); }
            restored
        } else {
            vec![Tab::new(home.clone(), view)]
        };

        let restored_active = if settings.remember_tabs {
            let session = config::load_session();
            session.active_tab.min(tabs.len().saturating_sub(1))
        } else { 0 };

        let egui_ctx = cc.egui_ctx.clone();
        let initial_theme_mtime = std::fs::metadata(config::theme_path()).and_then(|m| m.modified()).ok();

        let (theme_watch_tx, theme_watch_rx) = mpsc::channel();
        {
            let repaint = egui_ctx.clone();
            let mut last = initial_theme_mtime.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(2));
                let mt = std::fs::metadata(config::theme_path()).and_then(|m| m.modified()).ok();
                if mt != last {
                    last = mt.clone();
                    if theme_watch_tx.send(mt).is_err() { break; }
                    repaint.request_repaint();
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
                        if trash_watch_tx.send(count).is_err() { break; }
                        repaint.request_repaint();
                    }
                    std::thread::sleep(Duration::from_secs(3));
                }
            });
        }

        let mut app = Self {
            theme,
            egui_ctx: egui_ctx.clone(),
            theme_mtime: initial_theme_mtime,
            theme_check_rx: theme_watch_rx,
            settings: settings.clone(),
            tabs,
            active_tab: restored_active,
            entries: Vec::new(),
            entry_index: HashMap::new(),
            selected: None,
            selected_paths: HashSet::new(),
            search: String::new(),
            address: String::new(),
            show_hidden: settings.show_hidden,
            sort,
            sort_ascending: settings.sort_ascending,
            view,
            dirty: true,
            pinned: config::load_pinned(),
            mounts: Vec::new(),
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
            trash_count: 0,
            trash_count_rx: trash_watch_rx,
            metadata_rx: None,
            metadata_loading: false,
            file_op_rx: None,
            file_op_busy: false,
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
            delete_target: None,
            empty_trash_confirm: false,
        };
        app.sync_address();
        app.start_refresh();
        app
    }

    fn current_path(&self) -> &Path { &self.tabs[self.active_tab].path }
    fn rebuild_entry_index(&mut self) {
        self.entry_index = self.entries.iter().enumerate()
            .map(|(idx, entry)| (entry.path.clone(), idx))
            .collect();
    }
    fn in_trash(&self) -> bool { self.current_path() == fs_model::trash_files_dir() }
    fn sync_address(&mut self) { self.address = self.current_path().to_string_lossy().to_string(); }

    fn persist_session(&self) {
        if !self.settings.remember_tabs { return; }
        let state = config::SessionState {
            tabs: self.tabs.iter().map(|t| t.path.to_string_lossy().to_string()).collect(),
            active_tab: self.active_tab,
        };
        let _ = config::save_session(&state);
    }

    fn write_system_file_clipboard(&mut self, paths: &[PathBuf], mode: ClipboardMode) {
        if paths.is_empty() { return; }
        let action = if matches!(mode, ClipboardMode::Move) { "cut" } else { "copy" };
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
            if let Some(mut stdin) = child.stdin.take() { let _ = stdin.write_all(payload.as_bytes()); }
            let _ = child.wait();
        }
    }

    fn read_system_file_clipboard(&self) -> Option<ClipboardItem> {
        let output = Command::new("wl-paste")
            .args(["--no-newline", "--type", "x-special/gnome-copied-files"])
            .stderr(Stdio::null())
            .output().ok()?;
        if !output.status.success() { return None; }
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
        if paths.is_empty() { None } else { Some(ClipboardItem { paths, mode }) }
    }

    fn save_active_tab_state(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.active_tab) {
            tab.search = self.search.clone();
            tab.selected = self.selected.clone();
            tab.view = self.view;
        }
    }

    fn switch_to_tab(&mut self, idx: usize) {
        if idx >= self.tabs.len() || idx == self.active_tab { return; }
        self.save_active_tab_state();
        self.active_tab = idx;
        self.search = self.tabs[idx].search.clone();
        self.selected = self.tabs[idx].selected.clone();
        self.selected_paths.clear();
        if let Some(p) = self.selected.clone() { self.selected_paths.insert(p); }
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
            self.persist_session();
        }
    }

    fn close_tab(&mut self, idx: usize) {
        if self.tabs.len() <= 1 || idx >= self.tabs.len() { return; }
        self.save_active_tab_state();
        let closed = self.tabs.remove(idx);
        self.closed_tabs.push(closed);
        if self.closed_tabs.len() > 16 { self.closed_tabs.remove(0); }
        if idx < self.active_tab { self.active_tab -= 1; }
        else if self.active_tab >= self.tabs.len() { self.active_tab = self.tabs.len() - 1; }
        let tab = self.tabs[self.active_tab].clone();
        self.search = tab.search;
        self.selected = tab.selected;
        self.selected_paths.clear();
        if let Some(p) = self.selected.clone() { self.selected_paths.insert(p); }
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
            self.tabs.push(tab);
            self.active_tab = self.tabs.len() - 1;
            let tab = self.tabs[self.active_tab].clone();
            self.search = tab.search;
            self.selected = tab.selected;
            self.selected_paths.clear();
            if let Some(p) = self.selected.clone() { self.selected_paths.insert(p); }
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

    fn select_entry(&mut self, path: PathBuf, additive: bool) {
        if additive {
            if !self.selected_paths.insert(path.clone()) { self.selected_paths.remove(&path); }
            self.selected = if self.selected_paths.contains(&path) { Some(path) } else { self.selected_paths.iter().next().cloned() };
        } else {
            self.selected_paths.clear();
            self.selected_paths.insert(path.clone());
            self.selected = Some(path);
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
        self.search.clear();
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
            if let Some(home) = dirs::home_dir() { self.navigate(home); }
        } else if let Some(p) = self.current_path().parent() {
            self.navigate(p.to_path_buf());
        }
    }

    fn start_refresh(&mut self) {
        let path = self.current_path().to_path_buf();
        let show_hidden = self.show_hidden;
        let sort = self.sort;
        let sort_ascending = self.sort_ascending;
        let (tx, rx) = mpsc::channel();
        self.directory_rx = Some(rx);
        self.loading = true;
        self.dirty = false;
        self.status = format!("Loading {}…", path.file_name().and_then(|s| s.to_str()).unwrap_or("/"));

        let space_path = path.clone();
        let (space_tx, space_rx) = mpsc::channel();
        self.space_rx = Some(space_rx);
        self.current_space = None;
        let space_repaint = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let space = fs_model::disk_space(&space_path);
            if space_tx.send((space_path, space)).is_ok() { space_repaint.request_repaint(); }
        });

        let directory_repaint = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let (mut entries, error) = match fs_model::read_directory(&path, show_hidden) {
                Ok(entries) => (entries, None),
                Err(err) => (Vec::new(), Some(err.to_string())),
            };
            fs_model::sort_entries(&mut entries, sort, sort_ascending);
            let mounts = fs_model::discover_mounts();
            if tx.send(DirectoryLoadResult { path, entries, mounts, error }).is_ok() { directory_repaint.request_repaint(); }
        });
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
        let Some((path, space)) = result else { return; };
        self.space_rx = None;
        if path == self.current_path() { self.current_space = space; }
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

        let Some(result) = result else { return; };
        self.directory_rx = None;
        self.loading = false;

        if result.path != self.current_path() {
            return;
        }

        self.entries = result.entries;
        self.rebuild_entry_index();
        self.mounts = result.mounts;
        if let Some(error) = result.error {
            self.status = format!("Could not load {}: {error}", self.current_path().display());
        }
        let live_paths: HashSet<PathBuf> = self.entries.iter().map(|e| e.path.clone()).collect();
        self.thumbnails.retain(|key, _| live_paths.contains(&key.path) || self.selected.as_ref() == Some(&key.path));
        self.thumbnail_pending.retain(|key| live_paths.contains(&key.path));
        self.thumbnail_failed.retain(|key| live_paths.contains(&key.path));
        if self.status.starts_with("Loading ") {
            self.status = format!("{} items", self.entries.len());
        }
        if self.view == ViewMode::Details { self.start_metadata_load(); }
    }

    fn start_metadata_load(&mut self) {
        if self.metadata_loading || self.entries.is_empty() || (self.view != ViewMode::Details && !matches!(self.sort, SortColumn::Size | SortColumn::Modified)) { return; }
        let path = self.current_path().to_path_buf();
        let paths: Vec<PathBuf> = self.entries.iter()
            .filter(|e| !e.metadata_loaded)
            .map(|e| e.path.clone())
            .collect();
        if paths.is_empty() { return; }

        let (tx, rx) = mpsc::channel();
        self.metadata_rx = Some(rx);
        self.metadata_loading = true;
        let repaint = self.egui_ctx.clone();
        std::thread::spawn(move || {
            for chunk in paths.chunks(96) {
                let batch = fs_model::load_metadata(chunk);
                if tx.send(MetadataMessage::Batch(path.clone(), batch)).is_err() { return; }
                repaint.request_repaint();
            }
            if tx.send(MetadataMessage::Done(path)).is_ok() { repaint.request_repaint(); }
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
            if Instant::now() >= deadline { break; }
            let message = match self.metadata_rx.as_ref().map(|rx| rx.try_recv()) {
                Some(Ok(msg)) => Some(msg),
                Some(Err(TryRecvError::Disconnected)) => {
                    self.metadata_rx = None;
                    self.metadata_loading = false;
                    None
                }
                _ => None,
            };
            let Some(message) = message else { break; };
            processed += 1;
            match message {
                MetadataMessage::Batch(path, batch) => {
                    if path != self.current_path() { continue; }
                    for meta in batch {
                        if let Some(&idx) = self.entry_index.get(&meta.path) {
                            if let Some(entry) = self.entries.get_mut(idx) {
                                entry.size = meta.size;
                                entry.modified = meta.modified;
                                entry.accessed = meta.accessed;
                                entry.metadata_loaded = true;
                            }
                        }
                    }
                }
                MetadataMessage::Done(path) => {
                    if path == self.current_path() && matches!(self.sort, SortColumn::Size | SortColumn::Modified) {
                        fs_model::sort_entries(&mut self.entries, self.sort, self.sort_ascending);
                        self.rebuild_entry_index();
                    }
                    self.metadata_rx = None;
                    self.metadata_loading = false;
                    break;
                }
            }
        }
        if processed > 0 && self.metadata_rx.is_some() { self.egui_ctx.request_repaint(); }
    }

    fn persist_browsing_settings(&mut self) {
        self.settings.show_hidden = self.show_hidden;
        self.settings.default_view = if self.view == ViewMode::Icons { "icons".into() } else { "details".into() };
        self.settings.sort_column = match self.sort {
            SortColumn::Name => "name",
            SortColumn::Type => "type",
            SortColumn::Size => "size",
            SortColumn::Modified => "modified",
        }.into();
        self.settings.sort_ascending = self.sort_ascending;
        if let Err(e) = config::save_settings(&self.settings) {
            self.status = format!("Could not save settings: {e}");
        }
    }

    fn start_file_operation(&mut self, paths: Vec<PathBuf>, dest: PathBuf, mode: ClipboardMode) {
        if paths.is_empty() || self.file_op_busy { return; }
        let total = paths.len();
        let (tx, rx) = mpsc::channel();
        self.file_op_rx = Some(rx);
        self.file_op_busy = true;
        self.status = match mode { ClipboardMode::Copy => format!("Copying {total} item(s)…"), ClipboardMode::Move => format!("Moving {total} item(s)…") };

        let repaint = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let mut succeeded = 0usize;
            let mut failed = 0usize;
            for (idx, path) in paths.iter().enumerate() {
                let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("item").to_string();
                let result = match mode {
                    ClipboardMode::Copy => fs_model::copy_item(path, &dest),
                    ClipboardMode::Move => fs_model::move_item(path, &dest),
                };
                if result.is_ok() { succeeded += 1; } else { failed += 1; }
                if tx.send(FileOpMessage::Progress { done: idx + 1, total, name }).is_err() { return; }
                repaint.request_repaint();
            }
            if tx.send(FileOpMessage::Done { succeeded, failed, moved: matches!(mode, ClipboardMode::Move) }).is_ok() { repaint.request_repaint(); }
        });
    }

    fn poll_file_operations(&mut self) {
        const MAX_MESSAGES_PER_FRAME: usize = 8;
        let deadline = Instant::now() + Duration::from_millis(3);
        let mut processed = 0usize;
        for _ in 0..MAX_MESSAGES_PER_FRAME {
            if Instant::now() >= deadline { break; }
            let msg = match self.file_op_rx.as_ref().map(|rx| rx.try_recv()) {
                Some(Ok(msg)) => Some(msg),
                Some(Err(TryRecvError::Disconnected)) => {
                    self.file_op_rx = None;
                    self.file_op_busy = false;
                    None
                }
                _ => None,
            };
            let Some(msg) = msg else { break; };
            processed += 1;
            match msg {
                FileOpMessage::Progress { done, total, name } => {
                    self.status = format!("{done}/{total}: {name}");
                }
                FileOpMessage::Done { succeeded, failed, moved } => {
                    self.file_op_rx = None;
                    self.file_op_busy = false;
                    if moved {
                        self.clipboard = None;
                        let _ = Command::new("wl-copy")
                            .arg("--clear")
                            .stdin(Stdio::null())
                            .stdout(Stdio::null())
                            .stderr(Stdio::null())
                            .spawn();
                    }
                    self.status = if failed == 0 {
                        format!("Completed {succeeded} item(s)")
                    } else {
                        format!("Completed {succeeded} item(s); {failed} failed")
                    };
                    self.dirty = true;
                    break;
                }
            }
        }
        if processed > 0 && self.file_op_rx.is_some() { self.egui_ctx.request_repaint(); }
    }

    fn drop_drag_on(&mut self, dest: &Path, copy: bool) {
        let Some(payload) = self.drag_payload.take() else { return; };
        let mode = if copy { ClipboardMode::Copy } else { ClipboardMode::Move };
        self.start_file_operation(payload.paths, dest.to_path_buf(), mode);
    }

    fn handle_external_drops(&mut self, ctx: &egui::Context) {
        if self.file_op_busy { return; }
        let paths: Vec<PathBuf> = ctx.input(|i| {
            i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect()
        });
        if !paths.is_empty() {
            let dest = self.current_path().to_path_buf();
            let mode = if ctx.input(|i| i.modifiers.shift) { ClipboardMode::Move } else { ClipboardMode::Copy };
            self.start_file_operation(paths, dest, mode);
        }
    }

    fn filtered_indices(&self) -> Option<Vec<usize>> {
        let q = self.search.trim().to_ascii_lowercase();
        if q.is_empty() { return None; }
        Some(self.entries.iter().enumerate()
            .filter(|(_, e)| e.name.to_ascii_lowercase().contains(&q))
            .map(|(idx, _)| idx)
            .collect())
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
            self.navigate(entry.path.clone());
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

    fn open_new_window(&mut self, path: &Path) {
        let p = path.to_string_lossy().to_string();
        match Self::spawn_quiet("hoard", &[&p]) {
            Ok(_) => self.status = "Opened new Hoard window".into(),
            Err(e) => self.status = format!("Could not open new window: {e}"),
        }
    }

    fn pin_folder(&mut self, path: PathBuf) {
        let target = if path.is_dir() { path } else { path.parent().unwrap_or(self.current_path()).to_path_buf() };
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
        match Command::new(&self.settings.terminal).arg("--working-directory").arg(path).spawn() {
            Ok(_) => self.status = format!("Opened {}", self.settings.terminal),
            Err(e) => self.status = format!("Could not open terminal: {e}"),
        }
    }

    fn open_admin_terminal(&mut self, path: &Path) {
        let cmd = format!("cd {} && exec bash", shell_quote(path));
        match Command::new(&self.settings.terminal).args(["-e", "pkexec", "bash", "-lc", &cmd]).spawn() {
            Ok(_) => self.status = "Requested administrator terminal".into(),
            Err(e) => self.status = format!("Could not request administrator terminal: {e}"),
        }
    }

    fn check_updates(&mut self) {
        if self.update_rx.is_some() { return; }
        let (tx, rx) = mpsc::channel();
        self.update_rx = Some(rx);
        let repaint = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let msg = match updater::check_for_update() {
                Ok(Some(u)) => format!("GitHub update {} available — run: hoard --update", u.version),
                Ok(None) => "Hoard is up to date".into(),
                Err(e) => format!("Update check: {e}"),
            };
            if tx.send(msg).is_ok() { repaint.request_repaint(); }
        });
    }

    fn reload_theme_with_opacity(&mut self, ctx: &egui::Context) {
        self.theme = Theme::load(&config::theme_path())
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
        self.settings.default_view = if self.view == ViewMode::Icons { "icons".into() } else { "details".into() };
        self.settings.sort_column = match self.sort {
            SortColumn::Name => "name",
            SortColumn::Type => "type",
            SortColumn::Size => "size",
            SortColumn::Modified => "modified",
        }.into();
        self.settings.sort_ascending = self.sort_ascending;
        match config::save_settings(&self.settings) {
            Ok(_) => self.status = "Preferences saved".into(),
            Err(e) => self.status = format!("Could not save preferences: {e}"),
        }
    }

    fn keyboard(&mut self, ctx: &egui::Context) {
        let (ctrl, alt, shift, h, t, w, c, x, v, del, left, right, f5, tab_key) = ctx.input(|i| (
            i.modifiers.ctrl, i.modifiers.alt, i.modifiers.shift,
            i.key_pressed(egui::Key::H), i.key_pressed(egui::Key::T), i.key_pressed(egui::Key::W),
            i.key_pressed(egui::Key::C), i.key_pressed(egui::Key::X), i.key_pressed(egui::Key::V),
            i.key_pressed(egui::Key::Delete), i.key_pressed(egui::Key::ArrowLeft), i.key_pressed(egui::Key::ArrowRight),
            i.key_pressed(egui::Key::F5), i.key_pressed(egui::Key::Tab)
        ));

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
        if ctrl && w { self.close_tab(self.active_tab); }
        if ctrl && tab_key && !self.tabs.is_empty() {
            let next = if shift {
                (self.active_tab + self.tabs.len() - 1) % self.tabs.len()
            } else {
                (self.active_tab + 1) % self.tabs.len()
            };
            self.switch_to_tab(next);
        }
        if ctrl && c {
            let paths = self.selected_items();
            if !paths.is_empty() {
                self.clipboard = Some(ClipboardItem { paths: paths.clone(), mode: ClipboardMode::Copy });
                self.write_system_file_clipboard(&paths, ClipboardMode::Copy);
                self.status = format!("Copied {} item(s)", paths.len());
            }
        }
        if ctrl && x {
            let paths = self.selected_items();
            if !paths.is_empty() {
                self.clipboard = Some(ClipboardItem { paths: paths.clone(), mode: ClipboardMode::Move });
                self.write_system_file_clipboard(&paths, ClipboardMode::Move);
                self.status = format!("Cut {} item(s)", paths.len());
            }
        }
        if ctrl && v {
            let dest = self.current_path().to_path_buf();
            self.paste_into(&dest);
        }
        if del {
            let items = self.selected_items();
            if shift {
                if let Some(first) = items.first() { self.delete_target = Some(first.clone()); }
            } else {
                for p in items { self.move_to_trash(&p); }
            }
        }
        if alt && left { self.go_back(); }
        if alt && right { self.go_forward(); }
        if f5 { self.dirty = true; }
    }

    fn thumbnail_for(&mut self, _ctx: &egui::Context, path: &Path, max_side: u32) -> Option<TextureHandle> {
        if !self.settings.thumbnails || !fs_model::is_visual_media_file(path) { return None; }
        let key = thumbnail::ThumbnailKey { path: path.to_path_buf(), max_side: max_side.clamp(32, 512) };
        if let Some(tex) = self.thumbnails.get(&key) { return Some(tex.clone()); }
        if self.thumbnail_failed.contains(&key) { return None; }
        if self.thumbnail_pending.insert(key.clone()) && !self.thumbnail_loader.request(key.clone()) {
            self.thumbnail_pending.remove(&key);
        }
        None
    }

    fn poll_thumbnails(&mut self, ctx: &egui::Context) {
        let mut received = 0usize;
        const MAX_THUMBNAILS_PER_FRAME: usize = 3;
        let deadline = Instant::now() + Duration::from_millis(5);
        for _ in 0..MAX_THUMBNAILS_PER_FRAME {
            if Instant::now() >= deadline { break; }
            let Some(result) = self.thumbnail_loader.try_recv() else { break; };
            received += 1;
            self.thumbnail_pending.remove(&result.key);
            if let Some(image) = result.image {
                let tex = ctx.load_texture(
                    format!("thumb:{}:{}", result.key.path.display(), result.key.max_side),
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
            self.thumbnails.retain(|key, _| selected.as_ref() == Some(&key.path));
        }
        if received > 0 { ctx.request_repaint(); }
        if received == MAX_THUMBNAILS_PER_FRAME { self.egui_ctx.request_repaint(); }
    }

    fn move_to_trash(&mut self, path: &Path) {
        match fs_model::trash_item(path) {
            Ok(_) => { self.status = "Moved to Trash".into(); self.selected = None; self.dirty = true; }
            Err(e) => self.status = format!("Trash failed: {e}"),
        }
    }

    fn restore_from_trash(&mut self, path: &Path) {
        match fs_model::restore_trashed(path) {
            Ok(dest) => { self.status = format!("Restored to {}", dest.display()); self.selected = None; self.dirty = true; }
            Err(e) => self.status = format!("Restore failed: {e}"),
        }
    }

    fn paste_into(&mut self, dest: &Path) {
        let item = self.clipboard.clone().or_else(|| self.read_system_file_clipboard());
        let Some(item) = item else {
            self.status = "Clipboard has no files to paste".into();
            return;
        };
        if !dest.is_dir() { return; }
        self.start_file_operation(item.paths, dest.to_path_buf(), item.mode);
    }

    fn top_bar(&mut self, ctx: &egui::Context) {
        let t = self.theme.clone();
        egui::TopBottomPanel::top("title").exact_height(72.0).frame(
            egui::Frame::none().fill(t.sidebar).stroke(Stroke::new(1.0_f32, t.border))
        ).show(ctx, |ui| {
            let full = ui.max_rect();
            let drag = ui.interact(full, ui.id().with("drag-title"), Sense::click_and_drag());
            if drag.drag_started() { ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag); }

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
                    let tab_resp = egui::Frame::none()
                        .fill(fill)
                        .stroke(Stroke::new(1.0_f32, if active { t.accent } else { t.border }))
                        .rounding(7.0)
                        .inner_margin(egui::Margin::symmetric(10.0, 7.0))
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                let icon = if path == fs_model::trash_files_dir() { "♲" } else if path == dirs::home_dir().unwrap_or_default() { "⌂" } else { "▰" };
                                ui.label(RichText::new(icon).color(if active { t.accent_green } else { t.accent }));
                                ui.label(RichText::new(shorten(&label, 22)).strong().color(t.foreground));
                                if self.tabs.len() > 1 && ui.small_button("×").on_hover_text("Close tab").clicked() {
                                    close_tab = Some(idx);
                                }
                            });
                        }).response.interact(Sense::click());

                    if tab_resp.clicked() { self.switch_to_tab(idx); }
                    if tab_resp.clicked_by(egui::PointerButton::Middle) && self.tabs.len() > 1 { close_tab = Some(idx); }
                    if tab_resp.hovered() && ctx.input(|i| i.pointer.any_released()) && self.drag_payload.is_some() {
                        self.drop_drag_on(&path, ui.input(|i| i.modifiers.ctrl));
                    }
                    tab_resp.context_menu(|ui| {
                        if ui.button("Duplicate Tab").clicked() { duplicate_tab = Some(path.clone()); ui.close_menu(); }
                        if ui.button("Open Folder in New Window").clicked() { new_window = Some(path.clone()); ui.close_menu(); }
                        if ui.button("Copy Folder Path").clicked() { ui.ctx().copy_text(path.to_string_lossy().to_string()); ui.close_menu(); }
                        ui.separator();
                        if idx > 0 && ui.button("Move Tab Left").clicked() { move_left = Some(idx); ui.close_menu(); }
                        if idx + 1 < self.tabs.len() && ui.button("Move Tab Right").clicked() { move_right = Some(idx); ui.close_menu(); }
                        ui.separator();
                        if self.tabs.len() > 1 && ui.button("Close Tab").clicked() { close_tab = Some(idx); ui.close_menu(); }
                        if self.tabs.len() > 1 && ui.button("Close Other Tabs").clicked() { close_others = Some(idx); ui.close_menu(); }
                        if idx + 1 < self.tabs.len() && ui.button("Close Tabs to the Right").clicked() { close_right = Some(idx); ui.close_menu(); }
                    });
                }

                if let Some(path) = duplicate_tab { self.new_tab(path, true); }
                if let Some(path) = new_window { self.open_new_window(&path); }
                if let Some(idx) = move_left {
                    self.tabs.swap(idx, idx - 1);
                    if self.active_tab == idx { self.active_tab -= 1; }
                    else if self.active_tab == idx - 1 { self.active_tab += 1; }
                    self.persist_session();
                }
                if let Some(idx) = move_right {
                    self.tabs.swap(idx, idx + 1);
                    if self.active_tab == idx { self.active_tab += 1; }
                    else if self.active_tab == idx + 1 { self.active_tab -= 1; }
                    self.persist_session();
                }
                if let Some(idx) = close_right {
                    while self.tabs.len() > idx + 1 {
                        if let Some(tab) = self.tabs.pop() { self.closed_tabs.push(tab); }
                    }
                    self.active_tab = self.active_tab.min(self.tabs.len() - 1);
                    self.persist_session();
                    self.entries.clear(); self.entry_index.clear(); self.sync_address(); self.dirty = true;
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

                if ui.button(RichText::new("+").size(23.0)).on_hover_text("New tab  Ctrl+T").clicked() {
                    let p = self.current_path().to_path_buf();
                    self.new_tab(p, true);
                }

                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(8.0);
                    if ui.button(RichText::new("×").size(22.0).color(t.foreground)).clicked() { ctx.send_viewport_cmd(egui::ViewportCommand::Close); }
                    if ui.button("□").clicked() { self.maximized = !self.maximized; ctx.send_viewport_cmd(egui::ViewportCommand::Maximized(self.maximized)); }
                    if ui.button("—").clicked() { ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(true)); }
                });
            });
        });
    }

    fn paint_tool_icon(p: &egui::Painter, rect: Rect, icon: ToolIcon, color: Color32) {
        let c = rect.center();
        let w = rect.width().min(rect.height()) * 0.58;
        let s = Stroke::new(2.0_f32, color);
        match icon {
            ToolIcon::Back => {
                p.line_segment([Pos2::new(c.x + w*0.34, c.y), Pos2::new(c.x - w*0.26, c.y)], s);
                p.line_segment([Pos2::new(c.x - w*0.26, c.y), Pos2::new(c.x + w*0.02, c.y - w*0.30)], s);
                p.line_segment([Pos2::new(c.x - w*0.26, c.y), Pos2::new(c.x + w*0.02, c.y + w*0.30)], s);
            }
            ToolIcon::Forward => {
                p.line_segment([Pos2::new(c.x - w*0.34, c.y), Pos2::new(c.x + w*0.26, c.y)], s);
                p.line_segment([Pos2::new(c.x + w*0.26, c.y), Pos2::new(c.x - w*0.02, c.y - w*0.30)], s);
                p.line_segment([Pos2::new(c.x + w*0.26, c.y), Pos2::new(c.x - w*0.02, c.y + w*0.30)], s);
            }
            ToolIcon::Up => {
                p.line_segment([Pos2::new(c.x, c.y + w*0.35), Pos2::new(c.x, c.y - w*0.28)], s);
                p.line_segment([Pos2::new(c.x, c.y - w*0.28), Pos2::new(c.x - w*0.28, c.y)], s);
                p.line_segment([Pos2::new(c.x, c.y - w*0.28), Pos2::new(c.x + w*0.28, c.y)], s);
            }
            ToolIcon::Refresh => {
                p.circle_stroke(c, w*0.34, s);
                p.line_segment([Pos2::new(c.x+w*0.18,c.y-w*0.33), Pos2::new(c.x+w*0.39,c.y-w*0.32)], s);
                p.line_segment([Pos2::new(c.x+w*0.39,c.y-w*0.32), Pos2::new(c.x+w*0.34,c.y-w*0.11)], s);
            }
            ToolIcon::Home => {
                let roof = [Pos2::new(c.x-w*0.40,c.y-w*0.02), Pos2::new(c.x,c.y-w*0.36), Pos2::new(c.x+w*0.40,c.y-w*0.02)];
                p.add(egui::Shape::line(roof.to_vec(), s));
                p.rect_stroke(Rect::from_min_max(Pos2::new(c.x-w*0.28,c.y-w*0.02), Pos2::new(c.x+w*0.28,c.y+w*0.34)), 1.5, s);
            }
            ToolIcon::Details => {
                for y in [-0.27_f32, 0.0, 0.27] {
                    p.rect_filled(Rect::from_center_size(Pos2::new(c.x-w*0.34,c.y+w*y), Vec2::splat(3.2)), 1.0, color);
                    p.line_segment([Pos2::new(c.x-w*0.17,c.y+w*y), Pos2::new(c.x+w*0.38,c.y+w*y)], s);
                }
            }
            ToolIcon::Icons => {
                for (dx,dy) in [(-0.22_f32,-0.22_f32),(0.22,-0.22),(-0.22,0.22),(0.22,0.22)] {
                    p.rect_stroke(Rect::from_center_size(Pos2::new(c.x+w*dx,c.y+w*dy), Vec2::splat(w*0.28)), 2.0, s);
                }
            }
            ToolIcon::Cut => {
                p.circle_stroke(Pos2::new(c.x-w*0.22,c.y+w*0.25), w*0.13, s);
                p.circle_stroke(Pos2::new(c.x+w*0.22,c.y+w*0.25), w*0.13, s);
                p.line_segment([Pos2::new(c.x-w*0.12,c.y+w*0.11), Pos2::new(c.x+w*0.31,c.y-w*0.30)], s);
                p.line_segment([Pos2::new(c.x+w*0.12,c.y+w*0.11), Pos2::new(c.x-w*0.31,c.y-w*0.30)], s);
            }
            ToolIcon::Copy => {
                p.rect_stroke(Rect::from_min_max(Pos2::new(c.x-w*0.31,c.y-w*0.31), Pos2::new(c.x+w*0.13,c.y+w*0.13)), 2.0, s);
                p.rect_stroke(Rect::from_min_max(Pos2::new(c.x-w*0.08,c.y-w*0.08), Pos2::new(c.x+w*0.36,c.y+w*0.36)), 2.0, s);
            }
            ToolIcon::Paste => {
                p.rect_stroke(Rect::from_min_max(Pos2::new(c.x-w*0.31,c.y-w*0.16), Pos2::new(c.x+w*0.31,c.y+w*0.35)), 2.0, s);
                p.rect_stroke(Rect::from_center_size(Pos2::new(c.x,c.y-w*0.24), Vec2::new(w*0.34,w*0.18)), 2.0, s);
            }
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
        let width = if labels { (label.chars().count() as f32 * 7.1 + 39.0).max(62.0) } else { 34.0 };
        let (rect, response) = ui.allocate_exact_size(Vec2::new(width, 34.0), Sense::click());
        let fill = if selected { t.selection } else if response.hovered() { t.accent_soft } else { Color32::TRANSPARENT };
        let stroke = if selected { t.accent } else if response.hovered() { t.border } else { Color32::TRANSPARENT };
        ui.painter().rect_filled(rect, 7.0, fill);
        if stroke != Color32::TRANSPARENT {
            ui.painter().rect_stroke(rect, 7.0, Stroke::new(1.0_f32, stroke));
        }
        let icon_rect = if labels {
            Rect::from_min_size(Pos2::new(rect.left()+7.0, rect.top()+7.0), Vec2::splat(20.0))
        } else {
            Rect::from_center_size(rect.center(), Vec2::splat(20.0))
        };
        Self::paint_tool_icon(
            ui.painter(),
            icon_rect,
            icon,
            if selected || response.hovered() { t.accent_green } else { t.accent },
        );
        if labels {
            ui.painter().text(
                Pos2::new(rect.left()+32.0, rect.center().y),
                Align2::LEFT_CENTER,
                label,
                FontId::proportional(13.5),
                if selected { t.selection_text } else { t.foreground },
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
                ui.close_menu();
            }
            if ui.button("Blank File").clicked() {
                self.begin_new_file("New File");
                ui.close_menu();
            }
            ui.separator();
            if ui.button("Markdown Document  (.md)").clicked() {
                self.begin_new_file("New Document.md");
                ui.close_menu();
            }
            if ui.button("JSON File  (.json)").clicked() {
                self.begin_new_file("New File.json");
                ui.close_menu();
            }
            if ui.button("Shell Script  (.sh)").clicked() {
                self.begin_new_file("New Script.sh");
                ui.close_menu();
            }
            if ui.button("Python Script  (.py)").clicked() {
                self.begin_new_file("New Script.py");
                ui.close_menu();
            }
        });
        if ui.button("Folder").clicked() {
            self.show_new_folder = true;
            self.new_folder_name = "New Folder".into();
            ui.close_menu();
        }
    }

    fn folder_background_context_menu(&mut self, ui: &mut egui::Ui) {
        if ui.button("Open in New Window").clicked() {
            let path = self.current_path().to_path_buf();
            self.open_new_window(&path);
            ui.close_menu();
        }
        if ui.button("Open in New Tab").clicked() {
            let path = self.current_path().to_path_buf();
            self.new_tab(path, true);
            ui.close_menu();
        }
        ui.separator();
        ui.menu_button("New", |ui| self.new_item_menu(ui));
        if ui.button("Paste").clicked() {
            let dest = self.current_path().to_path_buf();
            self.paste_into(&dest);
            ui.close_menu();
        }
        ui.separator();
        if ui.button("Refresh").clicked() {
            self.dirty = true;
            ui.close_menu();
        }
        if ui.button("Open in Terminal").clicked() {
            let path = self.current_path().to_path_buf();
            self.open_terminal(&path);
            ui.close_menu();
        }
        if self.current_path() != Path::new("/") && !self.in_trash() {
            let path = self.current_path().to_path_buf();
            if self.pinned.contains(&path) {
                if ui.button("Unpin Current Folder").clicked() {
                    self.unpin_folder(&path);
                    ui.close_menu();
                }
            } else if ui.button("Pin Current Folder").clicked() {
                self.pin_folder(path);
                ui.close_menu();
            }
        }
    }

    fn toolbar(&mut self, ctx: &egui::Context) {
        let t = self.theme.clone();
        let toolbar_fill = t.panel;
        egui::TopBottomPanel::top("toolbar")
            .exact_height(54.0)
            .frame(
                egui::Frame::none()
                    .fill(toolbar_fill)
                    .stroke(Stroke::new(1.0_f32, t.border))
                    .inner_margin(egui::Margin::symmetric(10.0, 7.0)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let window_w = ctx.screen_rect().width();
                    let labels = self.settings.toolbar_labels && window_w >= 1560.0;
                    let compact = window_w < 1220.0;

                    // Navigation cluster.
                    egui::Frame::none()
                        .fill(t.panel_alt)
                        .stroke(Stroke::new(1.0_f32, t.border))
                        .rounding(8.0)
                        .inner_margin(egui::Margin::same(2.0))
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            if Self::toolbar_button(ui, ToolIcon::Back, "Back", false, false, &t).on_hover_text("Back  Alt+Left").clicked() { self.go_back(); }
                            if Self::toolbar_button(ui, ToolIcon::Forward, "Forward", false, false, &t).on_hover_text("Forward  Alt+Right").clicked() { self.go_forward(); }
                            if Self::toolbar_button(ui, ToolIcon::Up, "Up", false, false, &t).on_hover_text("Up one level").clicked() { self.go_up(); }
                            if Self::toolbar_button(ui, ToolIcon::Refresh, "Refresh", false, false, &t).on_hover_text("Refresh  F5").clicked() { self.dirty = true; }
                            let home = dirs::home_dir().unwrap_or_default();
                            if Self::toolbar_button(ui, ToolIcon::Home, "Home", false, false, &t).on_hover_text("Home").clicked() { self.navigate(home); }
                        });

                    // Keep location/search useful while allowing the command groups to stay
                    // on the same line. On narrower windows the address bar contracts first.
                    let total = ui.available_width();
                    let search_w = if window_w >= 1500.0 { 240.0 } else if window_w >= 1250.0 { 190.0 } else { 150.0 };
                    let fixed_commands = if labels { 610.0 } else if compact { 360.0 } else { 445.0 };
                    let min_addr = if compact { 170.0 } else { 220.0 };
                    let addr_w = (total - search_w - fixed_commands).clamp(min_addr, 680.0);

                    let response = ui.add_sized(
                        [addr_w, 36.0],
                        egui::TextEdit::singleline(&mut self.address).hint_text("Location"),
                    );
                    if response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        let p = PathBuf::from(self.address.trim());
                        self.navigate(p);
                    }

                    ui.add_sized(
                        [search_w, 36.0],
                        egui::TextEdit::singleline(&mut self.search).hint_text("Search"),
                    );

                    // File operation cluster.
                    egui::Frame::none()
                        .fill(t.panel_alt)
                        .stroke(Stroke::new(1.0_f32, t.border))
                        .rounding(8.0)
                        .inner_margin(egui::Margin::same(2.0))
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.x = 1.0;
                            if Self::toolbar_button(ui, ToolIcon::Cut, "Cut", labels, false, &t).on_hover_text("Ctrl+X").clicked() {
                                let paths = self.selected_items();
                                if !paths.is_empty() {
                                    self.clipboard = Some(ClipboardItem { paths: paths.clone(), mode: ClipboardMode::Move });
                                    self.write_system_file_clipboard(&paths, ClipboardMode::Move);
                                    self.status = format!("Cut {} item(s)", paths.len());
                                }
                            }
                            if Self::toolbar_button(ui, ToolIcon::Copy, "Copy", labels, false, &t).on_hover_text("Ctrl+C").clicked() {
                                let paths = self.selected_items();
                                if !paths.is_empty() {
                                    self.clipboard = Some(ClipboardItem { paths: paths.clone(), mode: ClipboardMode::Copy });
                                    self.write_system_file_clipboard(&paths, ClipboardMode::Copy);
                                    self.status = format!("Copied {} item(s)", paths.len());
                                }
                            }
                            if Self::toolbar_button(ui, ToolIcon::Paste, "Paste", labels, false, &t).on_hover_text("Ctrl+V").clicked() {
                                let dest = self.current_path().to_path_buf();
                                self.paste_into(&dest);
                            }
                        });

                    // View cluster.
                    egui::Frame::none()
                        .fill(t.panel_alt)
                        .stroke(Stroke::new(1.0_f32, t.border))
                        .rounding(8.0)
                        .inner_margin(egui::Margin::same(2.0))
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.x = 1.0;
                            if Self::toolbar_button(ui, ToolIcon::Details, "Details", labels, self.view == ViewMode::Details, &t).clicked() {
                                self.view = ViewMode::Details;
                                self.persist_browsing_settings();
                                self.start_metadata_load();
                            }
                            if Self::toolbar_button(ui, ToolIcon::Icons, "Icons", labels, self.view == ViewMode::Icons, &t).clicked() {
                                self.view = ViewMode::Icons;
                                self.persist_browsing_settings();
                            }
                        });

                    if !compact {
                        egui::Frame::none()
                            .fill(t.panel_alt)
                            .stroke(Stroke::new(1.0_f32, t.border))
                            .rounding(8.0)
                            .inner_margin(egui::Margin::symmetric(9.0, 5.0))
                            .show(ui, |ui| {
                                let hidden_changed = ui.checkbox(&mut self.show_hidden, "Hidden")
                                    .on_hover_text("Show hidden files  Ctrl+H")
                                    .changed();
                                if hidden_changed {
                                    self.dirty = true;
                                    self.persist_browsing_settings();
                                }
                            });
                    }

                    // Right-edge actions consume the remaining row width so the toolbar
                    // visually spans the window instead of ending in a dead strip.
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.menu_button(RichText::new(if compact { "☰" } else { "☰ Menu" }).strong(), |ui| {
                            let current = self.current_path().to_path_buf();
                            let selection = self.selected_items();
                            let has_selection = !selection.is_empty();

                            if ui.button("Open Current Folder in New Tab").clicked() { self.new_tab(current.clone(), true); ui.close_menu(); }
                            if ui.button("Open Current Folder in New Window").clicked() { self.open_new_window(&current); ui.close_menu(); }
                            ui.menu_button("New", |ui| self.new_item_menu(ui));

                            ui.separator();
                            if ui.add_enabled(has_selection, egui::Button::new("Cut    Ctrl+X")).clicked() {
                                self.clipboard = Some(ClipboardItem { paths: selection.clone(), mode: ClipboardMode::Move });
                                self.write_system_file_clipboard(&selection, ClipboardMode::Move);
                                self.status = format!("Cut {} item(s)", selection.len());
                                ui.close_menu();
                            }
                            if ui.add_enabled(has_selection, egui::Button::new("Copy    Ctrl+C")).clicked() {
                                self.clipboard = Some(ClipboardItem { paths: selection.clone(), mode: ClipboardMode::Copy });
                                self.write_system_file_clipboard(&selection, ClipboardMode::Copy);
                                self.status = format!("Copied {} item(s)", selection.len());
                                ui.close_menu();
                            }
                            if ui.button("Paste    Ctrl+V").clicked() {
                                let dest = self.current_path().to_path_buf();
                                self.paste_into(&dest);
                                ui.close_menu();
                            }

                            ui.separator();
                            if ui.button("Open in Terminal").clicked() { self.open_terminal(&current); ui.close_menu(); }
                            if compact {
                                let hidden_changed = ui.checkbox(&mut self.show_hidden, "Show hidden files").changed();
                                if hidden_changed { self.dirty = true; self.persist_browsing_settings(); }
                            }
                            if self.pinned.iter().any(|p| p == &current) {
                                if ui.button("Unpin Current Folder").clicked() { self.unpin_folder(&current); ui.close_menu(); }
                            } else if ui.button("Pin Current Folder").clicked() { self.pin_folder(current.clone()); ui.close_menu(); }
                            ui.separator();
                            if ui.button("Preferences…").clicked() { self.show_preferences = true; ui.close_menu(); }
                            if ui.button("Reload Theme").clicked() { self.reload_theme_with_opacity(ctx); ui.close_menu(); }
                            if ui.button("Check GitHub for Updates").clicked() { self.check_updates(); ui.close_menu(); }
                            if self.in_trash() {
                                ui.separator();
                                if ui.button(RichText::new("Empty Trash…").color(t.danger)).clicked() { self.empty_trash_confirm = true; ui.close_menu(); }
                            }
                            ui.separator();
                            ui.label(RichText::new(format!("Hoard {}", env!("CARGO_PKG_VERSION"))).color(t.muted));
                        });

                        ui.menu_button(RichText::new(if compact { "↕" } else { "↕ Sort" }).strong(), |ui| {
                            for (column, label) in [
                                (SortColumn::Name, "Name"),
                                (SortColumn::Type, "Type"),
                                (SortColumn::Size, "Size"),
                                (SortColumn::Modified, "Modified"),
                            ] {
                                if ui.selectable_label(self.sort == column, label).clicked() {
                                    self.sort = column;
                                    if matches!(column, SortColumn::Size | SortColumn::Modified) { self.start_metadata_load(); }
                                    fs_model::sort_entries(&mut self.entries, self.sort, self.sort_ascending);
                                    self.rebuild_entry_index();
                                    self.persist_browsing_settings();
                                    ui.close_menu();
                                }
                            }
                            ui.separator();
                            if ui.selectable_label(self.sort_ascending, "Ascending").clicked() {
                                self.sort_ascending = true;
                                fs_model::sort_entries(&mut self.entries, self.sort, self.sort_ascending);
                                self.rebuild_entry_index();
                                self.persist_browsing_settings();
                                ui.close_menu();
                            }
                            if ui.selectable_label(!self.sort_ascending, "Descending").clicked() {
                                self.sort_ascending = false;
                                fs_model::sort_entries(&mut self.entries, self.sort, self.sort_ascending);
                                self.rebuild_entry_index();
                                self.persist_browsing_settings();
                                ui.close_menu();
                            }
                        });

                        if self.file_op_busy { ui.spinner(); }
                    });
                });
            });
    }

    fn sidebar(&mut self, ctx: &egui::Context) {
        let t = self.theme.clone();
        let sidebar_fill = t.sidebar;
        egui::SidePanel::left("sidebar")
            .exact_width(t.metrics.sidebar_width)
            .resizable(false)
            .frame(
                egui::Frame::none()
                    // The SidePanel frame itself must carry the translucent surface.
                    // Painting only inside the child Ui leaves the scroll/frame gutter
                    // outside that clip fully transparent and creates a desktop-colored seam.
                    .fill(sidebar_fill)
                    .stroke(Stroke::new(1.0_f32, t.border))
                    .inner_margin(egui::Margin::symmetric(10.0, 0.0)),
            )
            .show(ctx, |ui| {
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
                            ui.label(RichText::new(self.pinned.len().to_string()).size(11.0).color(t.muted));
                        });
                    });
                    ui.add_space(4.0);
                    egui::Frame::none()
                        .fill(t.panel)
                        .stroke(Stroke::new(1.0_f32, t.border))
                        .rounding(8.0)
                        .inner_margin(egui::Margin::same(4.0))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            if self.pinned.is_empty() {
                                ui.label(RichText::new("Pin folders from the context menu for quick access.").size(12.0).color(t.muted));
                            } else {
                                for path in self.pinned.clone() {
                                    let label = path.file_name().and_then(|s| s.to_str()).unwrap_or("Pinned folder").to_string();
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

                    ui.add_space(14.0);
                    section_label(ui, "Network", t.muted);
                    ui.add_space(2.0);
                    self.sidebar_external(ui, "◎", "Network", "network:///");
                    self.sidebar_external(ui, "▰", "Windows Shares", "smb:///");
                    self.sidebar_external(ui, "▰", "SSH Locations", "sftp:///");
                    ui.add_space(12.0);
                });
            });
    }

    fn sidebar_path(&mut self, ui: &mut egui::Ui, icon: &str, label: &str, path: PathBuf) {
        let active = self.current_path() == path;
        let pinned = self.pinned.contains(&path);
        let fill = if active { self.theme.selection } else if pinned { self.theme.panel_alt } else { Color32::TRANSPARENT };
        let stroke = if active { self.theme.accent } else if pinned { self.theme.border } else { Color32::TRANSPARENT };
        let resp = egui::Frame::none()
            .fill(fill)
            .stroke(Stroke::new(1.0_f32, stroke))
            .rounding(7.0)
            .inner_margin(egui::Margin::symmetric(9.0, 7.0))
            .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(icon).color(if active { self.theme.accent_green } else { self.theme.accent }));
                ui.label(RichText::new(label).color(if active { self.theme.selection_text } else { self.theme.foreground }));
            });
        }).response.interact(Sense::click());
        if resp.clicked() { self.navigate(path.clone()); }
        let can_accept_drop = self.drag_payload.as_ref()
            .map(|p| !p.paths.contains(&path))
            .unwrap_or(false);
        let drop_hovered = can_accept_drop && resp.hovered();
        if drop_hovered {
            ui.painter().rect_stroke(resp.rect, 7.0, Stroke::new(1.5_f32, self.theme.accent_green));
        }
        if drop_hovered && ui.input(|i| i.pointer.any_released()) {
            self.drop_drag_on(&path, ui.input(|i| i.modifiers.ctrl));
        }
        resp.context_menu(|ui| {
            if ui.button("Open").clicked() { self.navigate(path.clone()); ui.close_menu(); }
            if ui.button("Open in New Tab").clicked() { self.new_tab(path.clone(), true); ui.close_menu(); }
            if ui.button("Open in New Window").clicked() { self.open_new_window(&path); ui.close_menu(); }
            if self.clipboard.is_some() && ui.button("Paste Into").clicked() { self.paste_into(&path); ui.close_menu(); }
            if path == fs_model::trash_files_dir() {
                ui.separator();
                if ui.button(RichText::new("Empty Trash…").color(self.theme.danger)).clicked() { self.empty_trash_confirm = true; ui.close_menu(); }
            } else if self.pinned.contains(&path) {
                if ui.button("Unpin Folder").clicked() { self.unpin_folder(&path); ui.close_menu(); }
            } else if path != PathBuf::from("/") {
                if ui.button("Pin Folder").clicked() { self.pin_folder(path.clone()); ui.close_menu(); }
            }
        });
    }

    fn sidebar_external(&mut self, ui: &mut egui::Ui, icon: &str, label: &str, uri: &str) {
        let r = ui.horizontal(|ui| { ui.label(RichText::new(icon).color(self.theme.accent)); ui.label(label); }).response.interact(Sense::click());
        if r.clicked() {
            let _ = Command::new("xdg-open").arg(uri).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn();
        }
    }

    fn central(&mut self, ctx: &egui::Context) {
        let t = self.theme.clone();
        egui::CentralPanel::default().frame(egui::Frame::none().fill(t.background)).show(ctx, |ui| {
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
                Pos2::new((available.max.x - preview_w).max(available.min.x), available.max.y),
            );
            let background = ui.interact(list_rect, ui.id().with("folder-background"), Sense::click());
            background.context_menu(|ui| self.folder_background_context_menu(ui));
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(list_rect), |ui| {
                if self.view == ViewMode::Details { self.details_view(ui); } else { self.icons_view(ui); }
            });

            if preview_w > 0.0 {
                let preview_rect = Rect::from_min_max(
                    Pos2::new(list_rect.max.x, available.min.y),
                    available.max,
                );
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(preview_rect), |ui| {
                    self.preview_panel(ui);
                });
            }
        });
    }

    fn details_view(&mut self, ui: &mut egui::Ui) {
        self.start_metadata_load();
        let width = ui.available_width();
        let name_x = 16.0;
        let type_x = width * 0.48;
        let size_x = width * 0.68;
        let modified_x = width * 0.80;
        let header_h = 40.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, header_h), Sense::hover());
        ui.painter().rect_filled(rect, 0.0, self.theme.panel_alt);
        ui.painter().line_segment([Pos2::new(rect.left(), rect.bottom()), Pos2::new(rect.right(), rect.bottom())], Stroke::new(1.0_f32, self.theme.border));
        self.header_text(ui, rect.left()+name_x, rect.center().y, "Name", SortColumn::Name);
        self.header_text(ui, rect.left()+type_x, rect.center().y, "Type", SortColumn::Type);
        self.header_text(ui, rect.left()+size_x, rect.center().y, "Size", SortColumn::Size);
        self.header_text(ui, rect.left()+modified_x, rect.center().y, "Modified", SortColumn::Modified);

        let filtered = self.filtered_indices();
        let row_count = filtered.as_ref().map_or(self.entries.len(), |indices| indices.len());
        if row_count == 0 {
            self.empty_folder_state(ui);
            return;
        }
        let row_h = self.theme.metrics.row_height.max(28.0);
        egui::ScrollArea::vertical().auto_shrink([false, false]).show_rows(ui, row_h, row_count, |ui, visible_rows| {
            for pos in visible_rows {
                let entry_idx = filtered.as_ref().and_then(|indices| indices.get(pos).copied()).unwrap_or(pos);
                let Some(entry) = self.entries.get(entry_idx).cloned() else { continue; };
                let selected = self.selected_paths.contains(&entry.path);
                let thumb = self.thumbnail_for(ui.ctx(), &entry.path, 64);
                let (row_rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), row_h), Sense::click_and_drag());
                let can_accept_drop = entry.is_dir
                    && self.drag_payload.as_ref().map(|p| !p.paths.contains(&entry.path)).unwrap_or(false);
                let drop_hovered = can_accept_drop && response.hovered();
                let fill = if drop_hovered { self.theme.accent_soft } else if selected { self.theme.selection } else if response.hovered() { self.theme.panel_alt } else { Color32::TRANSPARENT };
                let paint_rect = row_rect.shrink2(Vec2::new(2.0, 1.0));
                ui.painter().rect_filled(paint_rect, 4.0, fill);
                if drop_hovered {
                    ui.painter().rect_stroke(paint_rect, 4.0, Stroke::new(1.5_f32, self.theme.accent_green));
                }

                let icon_rect = Rect::from_min_size(Pos2::new(row_rect.left()+12.0, row_rect.center().y-14.0), Vec2::splat(28.0));
                if let Some(tex) = thumb {
                    ui.put(icon_rect, egui::Image::new((tex.id(), icon_rect.size())).rounding(3.0));
                } else if entry.is_dir {
                    paint_folder_icon(ui.painter(), icon_rect, self.theme.accent);
                } else {
                    paint_typed_file(ui.painter(), icon_rect, fs_model::icon_badge(&entry), self.theme.muted, self.theme.accent);
                }
                let text_color = if selected { self.theme.selection_text } else { self.theme.foreground };
                ui.painter().text(Pos2::new(row_rect.left()+48.0, row_rect.center().y), Align2::LEFT_CENTER, &entry.name, FontId::proportional(15.0), text_color);
                ui.painter().text(Pos2::new(row_rect.left()+type_x, row_rect.center().y), Align2::LEFT_CENTER, &entry.file_type, FontId::proportional(14.0), self.theme.foreground);
                let size_text = if entry.is_dir { "—".into() } else if entry.metadata_loaded { fs_model::format_size(entry.size) } else { "…".into() };
                let modified_text = if entry.metadata_loaded { fs_model::format_modified(entry.modified) } else { "…".into() };
                ui.painter().text(Pos2::new(row_rect.left()+size_x, row_rect.center().y), Align2::LEFT_CENTER, size_text, FontId::proportional(14.0), self.theme.muted);
                ui.painter().text(Pos2::new(row_rect.left()+modified_x, row_rect.center().y), Align2::LEFT_CENTER, modified_text, FontId::proportional(14.0), self.theme.foreground);

                if response.clicked() {
                    let additive = ui.input(|i| i.modifiers.ctrl);
                    self.select_entry(entry.path.clone(), additive);
                }
                if response.double_clicked() {
                    if entry.is_dir && ui.input(|i| i.modifiers.shift) { self.open_new_window(&entry.path); }
                    else { self.open_entry(&entry); }
                }
                if entry.is_dir && response.clicked_by(egui::PointerButton::Middle) { self.open_new_window(&entry.path); }
                if response.drag_started() {
                    if !self.selected_paths.contains(&entry.path) { self.select_entry(entry.path.clone(), false); }
                    self.drag_payload = Some(DragPayload { paths: self.selected_items() });
                }
                if drop_hovered && ui.input(|i| i.pointer.any_released()) {
                    self.drop_drag_on(&entry.path, ui.input(|i| i.modifiers.ctrl));
                }
                response.context_menu(|ui| self.entry_context_menu(ui, &entry));
            }
        });
    }

    fn header_text(&mut self, ui: &mut egui::Ui, x: f32, y: f32, label: &str, col: SortColumn) {
        let rect = Rect::from_min_size(Pos2::new(x-4.0, y-15.0), Vec2::new(120.0, 30.0));
        let r = ui.interact(rect, ui.id().with(label), Sense::click());
        if r.clicked() {
            if self.sort == col { self.sort_ascending = !self.sort_ascending; } else { self.sort = col; self.sort_ascending = true; }
            if matches!(col, SortColumn::Size | SortColumn::Modified) { self.start_metadata_load(); }
            fs_model::sort_entries(&mut self.entries, self.sort, self.sort_ascending);
            self.rebuild_entry_index();
            self.persist_browsing_settings();
        }
        let arrow = if self.sort == col { if self.sort_ascending { "⌃" } else { "⌄" } } else { "" };
        ui.painter().text(Pos2::new(x, y), Align2::LEFT_CENTER, format!("{}  {}", label, arrow), FontId::proportional(14.0), self.theme.foreground);
    }

    fn icons_view(&mut self, ui: &mut egui::Ui) {
        let filtered = self.filtered_indices();
        let item_count = filtered.as_ref().map_or(self.entries.len(), |indices| indices.len());
        if item_count == 0 {
            self.empty_folder_state(ui);
            return;
        }
        let thumb_size = self.settings.thumbnail_size.clamp(64, 192) as f32;
        let card_w = (thumb_size + 38.0).max(136.0);
        let card_h = thumb_size + 68.0;
        let gap = 12.0_f32;
        let available = ui.available_width().max(card_w);
        let columns = (((available + gap) / (card_w + gap)).floor() as usize).max(1);
        let rows = (item_count + columns - 1) / columns;
        let row_height = card_h + gap;

        egui::ScrollArea::vertical()
            .id_salt("icon-grid-scroll")
            .auto_shrink([false, false])
            .show_rows(ui, row_height, rows, |ui, visible_rows| {
                ui.set_min_width(available);
                for row in visible_rows {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = gap;
                        for col in 0..columns {
                            let pos = row * columns + col;
                            if pos >= item_count { break; }
                            let entry_idx = filtered.as_ref().and_then(|indices| indices.get(pos).copied()).unwrap_or(pos);
                            let Some(entry) = self.entries.get(entry_idx).cloned() else { break; };
                            let selected = self.selected_paths.contains(&entry.path);
                            let thumb = self.thumbnail_for(ui.ctx(), &entry.path, self.settings.thumbnail_size);
                            let (card_rect, response) = ui.allocate_exact_size(Vec2::new(card_w, card_h), Sense::click_and_drag());
                            let can_accept_drop = entry.is_dir
                                && self.drag_payload.as_ref().map(|p| !p.paths.contains(&entry.path)).unwrap_or(false);
                            let drop_hovered = can_accept_drop && response.hovered();
                            let fill = if drop_hovered { self.theme.accent_soft } else if selected { self.theme.selection } else if response.hovered() { self.theme.panel_alt } else { self.theme.panel };
                            let border = if drop_hovered { self.theme.accent_green } else if selected { self.theme.accent_green } else if response.hovered() { self.theme.accent } else { self.theme.border };
                            ui.painter().rect_filled(card_rect, 9.0, fill);
                            ui.painter().rect_stroke(card_rect, 9.0, Stroke::new(1.0_f32, border));

                            let image_rect = Rect::from_center_size(
                                Pos2::new(card_rect.center().x, card_rect.top() + 10.0 + thumb_size * 0.5),
                                Vec2::splat(thumb_size),
                            );
                            if let Some(tex) = thumb.clone() {
                                ui.put(image_rect, egui::Image::new((tex.id(), image_rect.size())).rounding(6.0));
                            } else if entry.is_dir {
                                paint_folder_icon(ui.painter(), image_rect.shrink(20.0), self.theme.accent);
                            } else {
                                paint_typed_file(ui.painter(), image_rect.shrink(22.0), fs_model::icon_badge(&entry), self.theme.muted, self.theme.accent);
                            }

                            ui.painter().text(
                                Pos2::new(card_rect.center().x, card_rect.bottom()-36.0),
                                Align2::CENTER_CENTER,
                                shorten(&entry.name, 24),
                                FontId::proportional(13.5),
                                if selected { self.theme.selection_text } else { self.theme.foreground },
                            );
                            ui.painter().text(
                                Pos2::new(card_rect.center().x, card_rect.bottom()-17.0),
                                Align2::CENTER_CENTER,
                                &entry.file_type,
                                FontId::proportional(10.5),
                                self.theme.muted,
                            );

                            if response.clicked() {
                                let additive = ui.input(|i| i.modifiers.ctrl);
                                self.select_entry(entry.path.clone(), additive);
                            }
                            if response.double_clicked() {
                                if entry.is_dir && ui.input(|i| i.modifiers.shift) { self.open_new_window(&entry.path); }
                                else { self.open_entry(&entry); }
                            }
                            if entry.is_dir && response.clicked_by(egui::PointerButton::Middle) { self.open_new_window(&entry.path); }
                            if response.drag_started() {
                                if !self.selected_paths.contains(&entry.path) { self.select_entry(entry.path.clone(), false); }
                                self.drag_payload = Some(DragPayload { paths: self.selected_items() });
                            }
                            if drop_hovered && ui.input(|i| i.pointer.any_released()) {
                                self.drop_drag_on(&entry.path, ui.input(|i| i.modifiers.ctrl));
                            }
                            response.context_menu(|ui| self.entry_context_menu(ui, &entry));
                        }
                    });
                }
            });
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
        let is_trash = fs_model::is_in_home_trash(&entry.path);
        let selection = if self.selected_paths.contains(&entry.path) {
            self.selected_items()
        } else {
            vec![entry.path.clone()]
        };
        let selection_count = selection.len();

        ui.label(RichText::new(&entry.name).strong().color(self.theme.foreground));
        ui.separator();
        if ui.button("Open").clicked() { self.open_entry(entry); ui.close_menu(); }
        if fs_model::is_image_file(&entry.path) {
            if ui.button("Open with Default Application").clicked() { self.open_with_default(&entry.path, &entry.name); ui.close_menu(); }
        }
        if !entry.is_dir && ui.button("Open With…").clicked() {
            self.open_with_chooser(&entry.path, &entry.name);
            ui.close_menu();
        }
        if entry.is_dir {
            if ui.button("Open in New Tab").clicked() { self.new_tab(entry.path.clone(), true); ui.close_menu(); }
            if ui.button("Open in New Window").clicked() { self.open_new_window(&entry.path); ui.close_menu(); }
        } else if let Some(parent) = entry.path.parent() {
            if ui.button("Open Parent in New Tab").clicked() { self.new_tab(parent.to_path_buf(), true); ui.close_menu(); }
        }

        ui.separator();
        let cut_label = if selection_count > 1 { format!("Cut {selection_count} Items") } else { "Cut".into() };
        let copy_label = if selection_count > 1 { format!("Copy {selection_count} Items") } else { "Copy".into() };
        if ui.button(cut_label).clicked() {
            self.clipboard = Some(ClipboardItem { paths: selection.clone(), mode: ClipboardMode::Move });
            self.write_system_file_clipboard(&selection, ClipboardMode::Move);
            self.status = format!("Cut {} item(s)", selection_count);
            ui.close_menu();
        }
        if ui.button(copy_label).clicked() {
            self.clipboard = Some(ClipboardItem { paths: selection.clone(), mode: ClipboardMode::Copy });
            self.write_system_file_clipboard(&selection, ClipboardMode::Copy);
            self.status = format!("Copied {} item(s)", selection_count);
            ui.close_menu();
        }
        if entry.is_dir && self.clipboard.is_some() && ui.button("Paste Into This Folder").clicked() { self.paste_into(&entry.path); ui.close_menu(); }
        if !is_trash && selection_count == 1 && ui.button("Duplicate").clicked() {
            match fs_model::duplicate_item(&entry.path) { Ok(_) => { self.status = "Duplicated item".into(); self.dirty = true; }, Err(e) => self.status = format!("Duplicate failed: {e}") }
            ui.close_menu();
        }

        ui.separator();
        if !is_trash && selection_count == 1 && ui.button("Rename…").clicked() {
            self.rename_target = Some(entry.path.clone());
            self.rename_text = entry.name.clone();
            ui.close_menu();
        }
        if ui.button("Copy Name").clicked() { ui.ctx().copy_text(entry.name.clone()); ui.close_menu(); }
        if ui.button("Copy Full Path").clicked() { ui.ctx().copy_text(entry.path.to_string_lossy().to_string()); ui.close_menu(); }
        if ui.button("Open in Terminal").clicked() {
            let p = if entry.is_dir { entry.path.clone() } else { entry.path.parent().unwrap_or(self.current_path()).to_path_buf() };
            self.open_terminal(&p);
            ui.close_menu();
        }

        if !is_trash {
            let pin_target = if entry.is_dir { entry.path.clone() } else { entry.path.parent().unwrap_or(self.current_path()).to_path_buf() };
            if self.pinned.contains(&pin_target) {
                if ui.button("Remove from Pinned").clicked() { self.unpin_folder(&pin_target); ui.close_menu(); }
            } else if ui.button("Pin Folder").clicked() { self.pin_folder(pin_target); ui.close_menu(); }
        }

        ui.separator();
        if is_trash {
            if ui.button("Restore").clicked() { self.restore_from_trash(&entry.path); ui.close_menu(); }
            if ui.button(RichText::new("Delete Permanently…").color(self.theme.danger)).clicked() { self.delete_target = Some(entry.path.clone()); ui.close_menu(); }
        } else if ui.button(RichText::new(if selection_count > 1 { "Move Selection to Trash" } else { "Move to Trash" }).color(self.theme.danger)).clicked() {
            for path in selection { self.move_to_trash(&path); }
            ui.close_menu();
        }

        ui.separator();
        if ui.button("Properties / Inspector").clicked() {
            self.select_entry(entry.path.clone(), false);
            self.status = "Properties shown in inspector".into();
            ui.close_menu();
        }
    }

    fn preview_panel(&mut self, ui: &mut egui::Ui) {
        let panel_rect = ui.max_rect();
        ui.painter().rect_filled(panel_rect, 0.0, self.theme.sidebar);
        ui.painter().line_segment(
            [panel_rect.left_top(), panel_rect.left_bottom()],
            Stroke::new(1.0_f32, self.theme.border),
        );

        let inner_rect = panel_rect.shrink2(Vec2::new(16.0, 14.0));
        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(inner_rect), |ui| {
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

                    if let Some(path) = self.selected.clone() {
                        let selected_entry = self.entry_index.get(&path)
                            .and_then(|idx| self.entries.get(*idx))
                            .cloned();
                        let is_dir = selected_entry.as_ref().map(|entry| entry.is_dir).unwrap_or(false);
                        let display_type = selected_entry.as_ref()
                            .map(|entry| entry.file_type.clone())
                            .unwrap_or_else(|| if is_dir { "Folder".into() } else { "File".into() });
                        let badge = selected_entry.as_ref().map(fs_model::icon_badge).unwrap_or("FILE");
                        let thumb = self.thumbnail_for(ui.ctx(), &path, 512);

                        // A dedicated preview canvas keeps every visual type aligned
                        // while preserving the source aspect ratio instead of forcing
                        // thumbnails into a fixed 220x160 rectangle.
                        let canvas_w = ui.available_width().max(120.0);
                        let canvas_h = (canvas_w * 0.68).clamp(150.0, 250.0);
                        let (canvas, _) = ui.allocate_exact_size(Vec2::new(canvas_w, canvas_h), Sense::hover());
                        ui.painter().rect_filled(canvas, 8.0, self.theme.panel_alt);
                        ui.painter().rect_stroke(canvas, 8.0, Stroke::new(1.0_f32, self.theme.border));

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
                            ui.put(image_rect, egui::Image::new((tex.id(), fitted)).rounding(6.0));
                        } else {
                            let icon_size = Vec2::new(
                                (canvas.width() * 0.42).clamp(92.0, 132.0),
                                (canvas.height() * 0.55).clamp(80.0, 118.0),
                            );
                            let icon_rect = Rect::from_center_size(canvas.center(), icon_size);
                            if is_dir {
                                paint_folder_icon(ui.painter(), icon_rect.shrink(10.0), self.theme.accent);
                            } else {
                                paint_typed_file(ui.painter(), icon_rect.shrink(10.0), badge, self.theme.muted, self.theme.accent);
                            }
                        }

                        ui.add_space(12.0);
                        ui.add(
                            egui::Label::new(
                                RichText::new(path.file_name().and_then(|s| s.to_str()).unwrap_or("Item"))
                                    .size(18.0)
                                    .strong(),
                            )
                            .wrap(),
                        );
                        ui.add_space(2.0);
                        ui.label(RichText::new(display_type).size(13.0).color(self.theme.muted));
                        ui.add_space(14.0);

                        egui::Frame::none()
                            .fill(self.theme.panel_alt)
                            .rounding(7.0)
                            .stroke(Stroke::new(1.0_f32, self.theme.border))
                            .inner_margin(egui::Margin::same(12.0))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.label(RichText::new("Details").strong());
                                ui.add_space(9.0);

                                preview_property(
                                    ui,
                                    "Location",
                                    path.parent().unwrap_or(Path::new("/")).to_string_lossy().as_ref(),
                                    self.theme.muted,
                                );

                                if let Some(entry) = &selected_entry {
                                    let size = if entry.is_dir {
                                        "—".into()
                                    } else if entry.metadata_loaded {
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
                                    preview_property(ui, "Modified", &modified, self.theme.muted);
                                    preview_property(ui, "Accessed", &accessed, self.theme.muted);
                                }
                            });

                        ui.add_space(12.0);
                        ui.label(RichText::new("Quick Actions").strong());
                        ui.add_space(6.0);

                        if let Some(entry) = selected_entry.clone() {
                            if !entry.is_dir && quick_button(ui, "↗", "Open", self.theme.panel_alt, self.theme.border).clicked() {
                                self.open_entry(&entry);
                            }
                        }
                        if is_dir && quick_button(ui, "▣", "Open in New Tab", self.theme.panel_alt, self.theme.border).clicked() {
                            self.new_tab(path.clone(), true);
                        }
                        if quick_button(ui, ">_", "Open in Terminal", self.theme.panel_alt, self.theme.border).clicked() {
                            let p = if is_dir { path.clone() } else { path.parent().unwrap_or(self.current_path()).to_path_buf() };
                            self.open_terminal(&p);
                        }
                        if quick_button(ui, "◆", "Open as Administrator", self.theme.panel_alt, self.theme.border).clicked() {
                            let p = if is_dir { path.clone() } else { path.parent().unwrap_or(self.current_path()).to_path_buf() };
                            self.open_admin_terminal(&p);
                        }
                        if fs_model::is_in_home_trash(&path) {
                            if quick_button(ui, "↶", "Restore", self.theme.panel_alt, self.theme.border).clicked() {
                                self.restore_from_trash(&path);
                            }
                        } else if is_dir {
                            let pinned = self.pinned.contains(&path);
                            let label = if pinned { "Unpin Folder" } else { "Pin Folder" };
                            let icon = if pinned { "♥" } else { "♡" };
                            if quick_button(ui, icon, label, self.theme.panel_alt, self.theme.border).clicked() {
                                if pinned { self.unpin_folder(&path); } else { self.pin_folder(path.clone()); }
                            }
                        }
                    } else {
                        ui.vertical_centered(|ui| {
                            ui.add_space(74.0);
                            self.paint_logo(ui, 76.0);
                            ui.add_space(10.0);
                            ui.label(RichText::new("Nothing selected").size(18.0).strong());
                            ui.add_space(2.0);
                            ui.label(RichText::new("Select a file or folder to preview it here.").color(self.theme.muted));
                        });
                    }
                });
        });
    }

    fn preferences_window(&mut self, ctx: &egui::Context) {
        if !self.show_preferences { return; }
        let mut open = self.show_preferences;
        let mut changed = false;
        let opacity_before = self.settings.window_opacity;
        let pref_panel = self.theme.panel_alt;
        let pref_border = self.theme.border;
        let pref_muted = self.theme.muted;
        let card = |ui: &mut egui::Ui, title: &str, subtitle: &str, body: &mut dyn FnMut(&mut egui::Ui)| {
            egui::Frame::none()
                .fill(pref_panel)
                .stroke(Stroke::new(1.0_f32, pref_border))
                .rounding(9.0)
                .inner_margin(egui::Margin::same(14.0))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new(title).size(16.0).strong());
                    ui.label(RichText::new(subtitle).size(12.5).color(pref_muted));
                    ui.add_space(10.0);
                    body(ui);
                });
        };

        egui::Window::new("Hoard Preferences")
            .open(&mut open)
            .resizable(true)
            .collapsible(false)
            .default_size([650.0, 610.0])
            .min_width(560.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    self.paint_logo(ui, 34.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Preferences").size(21.0).strong());
                        ui.label(RichText::new("Tune Hoard without changing the desktop around it.").color(self.theme.muted));
                    });
                });
                ui.add_space(10.0);

                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.set_width(ui.available_width());

                    card(ui, "Browsing", "Control the default file view and what Hoard displays.", &mut |ui| {
                        changed |= ui.checkbox(&mut self.settings.thumbnails, "Show thumbnails for visual media").changed();
                        changed |= ui.checkbox(&mut self.settings.show_preview, "Show the inspector / preview pane").changed();
                        changed |= ui.checkbox(&mut self.show_hidden, "Show hidden files").changed();
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label("Default view");
                            ui.add_space(8.0);
                            changed |= ui.selectable_value(&mut self.view, ViewMode::Details, "Details").changed();
                            changed |= ui.selectable_value(&mut self.view, ViewMode::Icons, "Icons").changed();
                        });
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label("Icon thumbnail size");
                            ui.add_space(8.0);
                            changed |= ui.add(egui::Slider::new(&mut self.settings.thumbnail_size, 64..=192).show_value(true)).changed();
                        });
                    });

                    ui.add_space(10.0);
                    card(ui, "Interface", "Keep the main window compact while preserving the tools you use.", &mut |ui| {
                        changed |= ui.checkbox(&mut self.settings.toolbar_labels, "Show toolbar text labels when space allows").changed();
                        ui.label(RichText::new("Toolbar labels automatically collapse on narrower windows so the location and search fields remain usable.").size(12.0).color(self.theme.muted));
                    });

                    ui.add_space(10.0);
                    card(ui, "Appearance", "Control how much of the desktop shows through Hoard without fading text or icons.", &mut |ui| {
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
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label("Terminal");
                            changed |= ui.add_sized([260.0, 28.0], egui::TextEdit::singleline(&mut self.settings.terminal)).changed();
                        });
                        ui.add_space(5.0);
                        ui.label(RichText::new("Shift + double-click or middle-click a folder to open it in a new window.").size(12.0).color(self.theme.muted));
                        ui.label(RichText::new("Drag items onto folders, tabs, or sidebar locations. Hold Ctrl while dropping to copy.").size(12.0).color(self.theme.muted));
                    });

                    ui.add_space(10.0);
                    card(ui, "Desktop compatibility", "Backend behavior selected for this session.", &mut |ui| {
                        let hyprland = std::env::var_os("HYPRLAND_INSTANCE_SIGNATURE").is_some();
                        if hyprland {
                            ui.label(RichText::new("Hyprland detected — default backend: XWayland compatibility mode").strong().color(self.theme.accent_green));
                            ui.label(RichText::new("Hoard uses the stable X11 presentation path at 1:1 UI scale to avoid the native Wayland EGL stall observed on hidden workspaces.").size(12.0).color(self.theme.muted));
                            ui.label(RichText::new("Use --native-wayland only for compatibility testing.").size(12.0).color(self.theme.muted));
                        } else {
                            ui.label(RichText::new("Automatic backend selection").strong().color(self.theme.accent_green));
                            ui.label(RichText::new("Hoard lets winit choose the platform backend on this desktop.").size(12.0).color(self.theme.muted));
                        }
                    });

                    ui.add_space(10.0);
                    card(ui, "Theme", "Edit the TOML theme directly or reload it after external changes.", &mut |ui| {
                        ui.add(egui::Label::new(RichText::new(config::theme_path().display().to_string()).size(12.0).color(self.theme.muted)).wrap());
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            if ui.button("Open Theme File").clicked() {
                                let p = config::theme_path().to_string_lossy().to_string();
                                let _ = Self::spawn_quiet("xdg-open", &[&p]);
                            }
                            if ui.button("Reload Theme").clicked() {
                                self.reload_theme_with_opacity(ctx);
                            }
                        });
                    });
                    ui.add_space(6.0);
                });

                ui.separator();
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Changes save automatically").color(self.theme.accent_green));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.button("Close").clicked() { self.show_preferences = false; }
                    });
                });
            });

        self.show_preferences = open && self.show_preferences;
        if changed {
            self.settings.window_opacity = self.settings.window_opacity.clamp(0.55, 1.0);
            if (self.settings.window_opacity - opacity_before).abs() > f32::EPSILON {
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
        self.preferences_window(ctx);

        if self.rename_target.is_some() {
            let target = self.rename_target.clone().unwrap();
            let mut open = true;
            egui::Window::new("Rename").open(&mut open).resizable(false).collapsible(false).show(ctx, |ui| {
                ui.label(format!("Rename {}", target.file_name().and_then(|s| s.to_str()).unwrap_or("item")));
                let r = ui.text_edit_singleline(&mut self.rename_text);
                if r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) || ui.button("Rename").clicked() {
                    match fs_model::rename_item(&target, &self.rename_text) {
                        Ok(p) => { self.status = format!("Renamed to {}", p.file_name().and_then(|s| s.to_str()).unwrap_or("item")); self.rename_target = None; self.selected = Some(p); self.dirty = true; }
                        Err(e) => self.status = format!("Rename failed: {e}"),
                    }
                }
            });
            if !open { self.rename_target = None; }
        }

        if self.show_new_folder {
            let mut open = self.show_new_folder;
            egui::Window::new("New Folder").open(&mut open).resizable(false).collapsible(false).show(ctx, |ui| {
                ui.label(RichText::new("Folder name").size(12.0).color(self.theme.muted));
                let name = ui.add_sized([360.0, 30.0], egui::TextEdit::singleline(&mut self.new_folder_name));
                name.request_focus();
                let submit = ui.input(|i| i.key_pressed(egui::Key::Enter)) || ui.button("Create Folder").clicked();
                if submit {
                    let trimmed = self.new_folder_name.trim();
                    if trimmed.is_empty() {
                        self.status = "Folder name cannot be empty".into();
                    } else {
                        match fs_model::create_folder(self.current_path(), trimmed) {
                            Ok(p) => { self.status = format!("Created {}", p.display()); self.show_new_folder = false; self.dirty = true; }
                            Err(e) => self.status = format!("Create folder failed: {e}"),
                        }
                    }
                }
            });
            if !open { self.show_new_folder = false; }
        }

        if self.show_new_file {
            let mut open = self.show_new_file;
            egui::Window::new("New File").open(&mut open).resizable(false).collapsible(false).show(ctx, |ui| {
                ui.label(RichText::new("File name").size(12.0).color(self.theme.muted));
                let name = ui.add_sized([360.0, 30.0], egui::TextEdit::singleline(&mut self.new_file_name));
                name.request_focus();
                let submit = ui.input(|i| i.key_pressed(egui::Key::Enter)) || ui.button("Create File").clicked();
                if submit {
                    let trimmed = self.new_file_name.trim();
                    if trimmed.is_empty() {
                        self.status = "File name cannot be empty".into();
                    } else {
                        match fs_model::create_text_file(self.current_path(), trimmed) {
                            Ok(p) => { self.status = format!("Created {}", p.display()); self.show_new_file = false; self.dirty = true; }
                            Err(e) => self.status = format!("Create file failed: {e}"),
                        }
                    }
                }
            });
            if !open { self.show_new_file = false; }
        }

        if let Some(target) = self.delete_target.clone() {
            if !self.settings.confirm_permanent_delete {
                match fs_model::delete_permanently(&target) { Ok(_) => self.status = "Deleted permanently".into(), Err(e) => self.status = format!("Delete failed: {e}") }
                self.delete_target = None; self.selected = None; self.dirty = true;
            } else {
                let mut open = true;
                egui::Window::new("Delete Permanently?").open(&mut open).resizable(false).collapsible(false).show(ctx, |ui| {
                    ui.label("This cannot be undone.");
                    ui.label(RichText::new(target.to_string_lossy()).color(self.theme.muted));
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() { self.delete_target = None; }
                        if ui.button(RichText::new("Delete Permanently").color(self.theme.danger)).clicked() {
                            match fs_model::delete_permanently(&target) { Ok(_) => self.status = "Deleted permanently".into(), Err(e) => self.status = format!("Delete failed: {e}") }
                            self.delete_target = None; self.selected = None; self.dirty = true;
                        }
                    });
                });
                if !open { self.delete_target = None; }
            }
        }

        if self.empty_trash_confirm {
            let mut open = true;
            egui::Window::new("Empty Trash?").open(&mut open).resizable(false).collapsible(false).show(ctx, |ui| {
                ui.label(format!("Permanently delete {} item(s) from Trash?", self.trash_count));
                ui.label(RichText::new("This cannot be undone.").color(self.theme.danger));
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() { self.empty_trash_confirm = false; }
                    if ui.button(RichText::new("Empty Trash").color(self.theme.danger)).clicked() {
                        match fs_model::empty_trash() { Ok(_) => self.status = "Trash emptied".into(), Err(e) => self.status = format!("Could not empty Trash: {e}") }
                        self.empty_trash_confirm = false; self.selected = None; self.dirty = true;
                    }
                });
            });
            if !open { self.empty_trash_confirm = false; }
        }
    }

    fn bottom_bar(&mut self, ctx: &egui::Context) {
        let t = self.theme.clone();
        egui::TopBottomPanel::bottom("status").exact_height(46.0).frame(
            egui::Frame::none()
                .fill(t.sidebar)
                .stroke(Stroke::new(1.0_f32, t.border))
                .inner_margin(egui::Margin::symmetric(10.0, 6.0))
        ).show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(t.metrics.sidebar_width + 6.0);
                let selected = self.selected_paths.len();
                ui.label(RichText::new(format!("{} selected", selected)).color(t.muted));
                ui.separator(); ui.label(RichText::new(format!("{} items", self.entries.len())).color(t.muted));
                if self.loading { ui.separator(); ui.spinner(); ui.label(RichText::new("Loading…").color(t.accent)); }
                if self.in_trash() { ui.separator(); ui.label(RichText::new(format!("Trash: {} item(s)", self.trash_count)).color(t.accent)); }
                if let Some(payload) = &self.drag_payload {
                    let copy = ctx.input(|i| i.modifiers.ctrl);
                    ui.separator();
                    ui.label(
                        RichText::new(format!("{} {} item(s) — release over a highlighted folder", if copy { "Copy" } else { "Move" }, payload.paths.len()))
                            .color(t.accent_green),
                    );
                } else if !self.status.is_empty() {
                    ui.separator();
                    ui.label(RichText::new(&self.status).color(t.accent_green));
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some((free, total)) = self.current_space {
                        let frac = if total == 0 { 0.0 } else { 1.0 - (free as f32 / total as f32) };
                        ui.add(egui::ProgressBar::new(frac).desired_width(210.0).text(format!("{} free ({})", fs_model::format_size(free), fs_model::format_size(total))));
                    }
                    ui.separator();
                    ui.label(RichText::new("Icon size").color(t.muted));
                    let slider_resp = ui.add_sized(
                        [108.0, 18.0],
                        egui::Slider::new(&mut self.settings.thumbnail_size, 64..=192)
                            .show_value(false)
                            .step_by(8.0)
                            .clamping(egui::SliderClamping::Always)
                    ).on_hover_text("Adjust icon thumbnail size");
                    ui.label(RichText::new(format!("{}", self.settings.thumbnail_size)).color(t.muted));
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
            Pos2::new(c.x, c.y - 17.0*u),
            Pos2::new(c.x + 12.0*u, c.y - 8.0*u),
            Pos2::new(c.x + 9.0*u, c.y + 8.0*u),
            Pos2::new(c.x, c.y + 17.0*u),
            Pos2::new(c.x - 9.0*u, c.y + 8.0*u),
            Pos2::new(c.x - 12.0*u, c.y - 8.0*u),
        ];
        p.add(egui::Shape::convex_polygon(head, self.theme.accent_soft, Stroke::new(2.2_f32*u, self.theme.accent)));

        // Horns / crown.
        p.line_segment([Pos2::new(c.x-9.0*u,c.y-8.0*u), Pos2::new(c.x-16.0*u,c.y-16.0*u)], Stroke::new(2.6_f32*u,self.theme.accent));
        p.line_segment([Pos2::new(c.x-16.0*u,c.y-16.0*u), Pos2::new(c.x-13.0*u,c.y-5.0*u)], Stroke::new(2.2_f32*u,self.theme.accent));
        p.line_segment([Pos2::new(c.x+9.0*u,c.y-8.0*u), Pos2::new(c.x+16.0*u,c.y-16.0*u)], Stroke::new(2.6_f32*u,self.theme.accent));
        p.line_segment([Pos2::new(c.x+16.0*u,c.y-16.0*u), Pos2::new(c.x+13.0*u,c.y-5.0*u)], Stroke::new(2.2_f32*u,self.theme.accent));

        // Brow / muzzle geometry.
        p.line_segment([Pos2::new(c.x-8.0*u,c.y-4.0*u), Pos2::new(c.x-2.0*u,c.y-1.0*u)], Stroke::new(2.0_f32*u,self.theme.accent_green));
        p.line_segment([Pos2::new(c.x+8.0*u,c.y-4.0*u), Pos2::new(c.x+2.0*u,c.y-1.0*u)], Stroke::new(2.0_f32*u,self.theme.accent_green));
        p.line_segment([Pos2::new(c.x,c.y-1.0*u), Pos2::new(c.x,c.y+9.0*u)], Stroke::new(1.8_f32*u,self.theme.accent));
        p.line_segment([Pos2::new(c.x-5.0*u,c.y+7.0*u), Pos2::new(c.x,c.y+11.0*u)], Stroke::new(1.8_f32*u,self.theme.accent));
        p.line_segment([Pos2::new(c.x+5.0*u,c.y+7.0*u), Pos2::new(c.x,c.y+11.0*u)], Stroke::new(1.8_f32*u,self.theme.accent));

        // Small triangular eyes.
        let left_eye = vec![Pos2::new(c.x-7.0*u,c.y-3.0*u), Pos2::new(c.x-3.0*u,c.y-2.0*u), Pos2::new(c.x-5.0*u,c.y+0.5*u)];
        let right_eye = vec![Pos2::new(c.x+7.0*u,c.y-3.0*u), Pos2::new(c.x+3.0*u,c.y-2.0*u), Pos2::new(c.x+5.0*u,c.y+0.5*u)];
        p.add(egui::Shape::convex_polygon(left_eye, self.theme.accent_green, Stroke::NONE));
        p.add(egui::Shape::convex_polygon(right_eye, self.theme.accent_green, Stroke::NONE));
    }
}

impl eframe::App for HoardApp {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Native viewport transparency is enabled in main.rs. Keeping the clear
        // buffer fully transparent lets the alpha on Hoard's structural panels
        // composite against the desktop instead of an opaque GL clear color.
        [0.0, 0.0, 0.0, 0.0]
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.hot_reload_theme(ctx);
        self.poll_trash_count();
        self.keyboard(ctx);
        self.handle_external_drops(ctx);
        if self.dirty { self.start_refresh(); }
        self.poll_directory();
        self.poll_space();
        self.poll_metadata();
        self.poll_file_operations();
        self.poll_thumbnails(ctx);
        if let Some(rx) = &self.update_rx { if let Ok(msg) = rx.try_recv() { self.status = msg; self.update_rx = None; } }
        if let Some(rx) = &self.open_with_rx { if let Ok(msg) = rx.try_recv() { self.status = msg; self.open_with_rx = None; } }

        self.top_bar(ctx);
        self.toolbar(ctx);
        self.bottom_bar(ctx);
        self.sidebar(ctx);
        self.central(ctx);
        self.dialogs(ctx);

        if ctx.input(|i| i.pointer.any_released()) {
            self.drag_payload = None;
        }
        if self.dirty { ctx.request_repaint(); }

        let screen = ctx.screen_rect();
        ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("outer-border")))
            .rect_stroke(screen.shrink(1.0), 9.0, Stroke::new(1.0_f32, self.theme.accent));
    }
}

fn section_label(ui: &mut egui::Ui, text: &str, color: Color32) { ui.label(RichText::new(text).strong().color(color)); }
fn preview_property(ui: &mut egui::Ui, key: &str, value: &str, muted: Color32) {
    ui.label(RichText::new(key).size(12.0).color(muted));
    ui.add(egui::Label::new(RichText::new(value).size(13.5)).wrap());
    ui.add_space(8.0);
}
fn quick_button(ui: &mut egui::Ui, icon: &str, label: &str, fill: Color32, border: Color32) -> egui::Response { ui.add_sized([ui.available_width(), 36.0], egui::Button::new(format!("{}    {}", icon, label)).fill(fill).stroke(Stroke::new(1.0_f32, border))) }

fn paint_folder_icon(p: &egui::Painter, rect: Rect, color: Color32) {
    let dark = Color32::from_rgb(
        color.r().saturating_sub(35),
        color.g().saturating_sub(30),
        color.b().saturating_sub(20),
    );
    let body_w = (rect.width() * 0.78).max(20.0);
    let body_h = (rect.height() * 0.50).max(14.0);
    let body = Rect::from_center_size(
        Pos2::new(rect.center().x, rect.center().y + rect.height()*0.08),
        Vec2::new(body_w, body_h),
    );
    let tab_w = body_w * 0.42;
    let tab_h = body_h * 0.34;
    let tab = Rect::from_min_size(
        Pos2::new(body.left() + body_w*0.08, body.top() - tab_h*0.72),
        Vec2::new(tab_w, tab_h),
    );
    p.rect_filled(tab, 3.0, dark);
    p.rect_filled(body, 5.0, color);
    p.rect_stroke(body, 5.0, Stroke::new(1.0_f32, dark));
}
fn paint_typed_file(p: &egui::Painter, rect: Rect, badge: &str, file_color: Color32, accent: Color32) {
    let w = rect.width().max(18.0);
    let h = rect.height().max(22.0);
    let page_w = (w * 0.72).min(54.0);
    let page_h = (h * 0.82).min(68.0);
    let page = Rect::from_center_size(rect.center(), Vec2::new(page_w, page_h));
    let fold = (page_w * 0.23).clamp(4.0, 12.0);

    p.rect_filled(page, 4.0, file_color);
    p.rect_stroke(page, 4.0, Stroke::new(1.0_f32, accent));
    p.add(egui::Shape::convex_polygon(
        vec![
            Pos2::new(page.right()-fold, page.top()),
            Pos2::new(page.right(), page.top()),
            Pos2::new(page.right(), page.top()+fold),
        ],
        accent,
        Stroke::NONE,
    ));

    let font = if page_w < 28.0 { 7.0 } else if badge.len() > 3 { 9.0 } else { 11.0 };
    p.text(
        Pos2::new(page.center().x, page.center().y + page_h*0.08),
        Align2::CENTER_CENTER,
        badge,
        FontId::proportional(font),
        Color32::WHITE,
    );
}
fn shorten(s: &str, max: usize) -> String { if s.chars().count() <= max { s.to_string() } else { format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>()) } }
fn shell_quote(path: &Path) -> String { let s = path.to_string_lossy(); format!("'{}'", s.replace('\'', "'\\''")) }
