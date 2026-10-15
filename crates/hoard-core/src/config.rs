use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const DEFAULT_GITHUB_REPOSITORY: &str = "diedemus/hoard";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_repository")]
    pub repository: String,
    #[serde(default)]
    pub manifest_url: String,
    #[serde(default = "default_channel")]
    pub channel: String,
    #[serde(default = "default_interval")]
    pub check_interval_hours: u64,
}

fn default_provider() -> String {
    "github".into()
}
fn default_repository() -> String {
    DEFAULT_GITHUB_REPOSITORY.into()
}
fn default_channel() -> String {
    "stable".into()
}
fn default_interval() -> u64 {
    24
}
fn default_true() -> bool {
    true
}
fn default_cache_limit_mb() -> u64 {
    768
}
fn default_recursive_max_results() -> usize {
    5000
}
fn default_details_columns() -> Vec<String> {
    ["name", "type", "size", "modified"]
        .into_iter()
        .map(str::to_string)
        .collect()
}
fn default_details_column_widths() -> BTreeMap<String, f32> {
    [
        ("name", 320.0),
        ("type", 150.0),
        ("size", 110.0),
        ("modified", 190.0),
        ("created", 190.0),
        ("owner", 110.0),
        ("permissions", 120.0),
    ]
    .into_iter()
    .map(|(key, width)| (key.to_string(), width))
    .collect()
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            provider: default_provider(),
            repository: default_repository(),
            manifest_url: String::new(),
            channel: default_channel(),
            check_interval_hours: default_interval(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub show_hidden: bool,
    #[serde(default = "default_view")]
    pub default_view: String,
    #[serde(default = "default_true")]
    pub thumbnails: bool,
    #[serde(default = "default_thumbnail_size")]
    pub thumbnail_size: u32,
    #[serde(default = "default_true")]
    pub show_preview: bool,
    #[serde(default = "default_true")]
    pub toolbar_labels: bool,
    #[serde(default = "default_window_opacity")]
    pub window_opacity: f32,
    #[serde(default = "default_theme_preset")]
    pub theme_preset: String,
    #[serde(default = "default_true")]
    pub confirm_permanent_delete: bool,
    #[serde(default = "default_terminal")]
    pub terminal: String,
    #[serde(default = "default_sort_column")]
    pub sort_column: String,
    #[serde(default = "default_true")]
    pub sort_ascending: bool,
    #[serde(default = "default_true")]
    pub remember_tabs: bool,
    #[serde(default)]
    pub open_folders_in_new_tab: bool,
    #[serde(default)]
    pub recursive_search_default: bool,
    #[serde(default = "default_recursive_max_results")]
    pub recursive_search_max_results: usize,
    #[serde(default = "default_true")]
    pub directory_cache: bool,
    #[serde(default = "default_true")]
    pub thumbnail_disk_cache: bool,
    #[serde(default = "default_cache_limit_mb")]
    pub thumbnail_cache_limit_mb: u64,
    #[serde(default = "default_cache_max_age_days")]
    pub thumbnail_cache_max_age_days: u64,
    #[serde(default)]
    pub recursive_search_remote: bool,
    #[serde(default = "default_details_columns")]
    pub details_columns: Vec<String>,
    #[serde(default = "default_details_column_widths")]
    pub details_column_widths: BTreeMap<String, f32>,
}

fn default_view() -> String {
    "details".into()
}
fn default_thumbnail_size() -> u32 {
    112
}
fn default_window_opacity() -> f32 {
    1.0
}
fn default_theme_preset() -> String {
    "obsidian-violet".into()
}
fn default_terminal() -> String {
    "alacritty".into()
}
fn default_sort_column() -> String {
    "name".into()
}
fn default_cache_max_age_days() -> u64 {
    30
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            show_hidden: false,
            default_view: default_view(),
            thumbnails: true,
            thumbnail_size: default_thumbnail_size(),
            show_preview: true,
            toolbar_labels: true,
            window_opacity: default_window_opacity(),
            theme_preset: default_theme_preset(),
            confirm_permanent_delete: true,
            terminal: default_terminal(),
            sort_column: default_sort_column(),
            sort_ascending: true,
            remember_tabs: true,
            open_folders_in_new_tab: false,
            recursive_search_default: false,
            recursive_search_max_results: default_recursive_max_results(),
            directory_cache: true,
            thumbnail_disk_cache: true,
            thumbnail_cache_limit_mb: default_cache_limit_mb(),
            thumbnail_cache_max_age_days: default_cache_max_age_days(),
            recursive_search_remote: false,
            details_columns: default_details_columns(),
            details_column_widths: default_details_column_widths(),
        }
    }
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("hoard")
}

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from(".cache"))
        .join("hoard")
}

pub fn state_dir() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local/state")
        })
        .join("hoard")
}

pub fn operation_journal_path() -> PathBuf {
    state_dir().join("operation-in-progress.txt")
}
pub fn crash_log_path() -> PathBuf {
    state_dir().join("crash.log")
}

fn atomic_write(path: &Path, body: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(body)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn write_operation_journal(description: &str) {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let _ = atomic_write(
        &operation_journal_path(),
        format!("started={stamp}\noperation={description}\n").as_bytes(),
    );
}

pub fn clear_operation_journal() {
    let _ = fs::remove_file(operation_journal_path());
}

pub fn interrupted_operation() -> Option<String> {
    fs::read_to_string(operation_journal_path()).ok()
}

pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let _ = fs::create_dir_all(state_dir());
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let line = format!("[{stamp}] {info}\n");
        if let Ok(mut f) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(crash_log_path())
        {
            let _ = f.write_all(line.as_bytes());
        }
        eprintln!("{info}");
    }));
}

pub fn theme_path() -> PathBuf {
    config_dir().join("themes/hoard.toml")
}
pub fn update_config_path() -> PathBuf {
    config_dir().join("update.toml")
}
pub fn settings_path() -> PathBuf {
    config_dir().join("settings.toml")
}
pub fn session_path() -> PathBuf {
    config_dir().join("session.toml")
}
pub fn pinned_path() -> PathBuf {
    config_dir().join("pinned.txt")
}
pub fn legacy_bookmarks_path() -> PathBuf {
    config_dir().join("bookmarks.txt")
}

pub fn load_update_config() -> UpdateConfig {
    fs::read_to_string(update_config_path())
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_update_config(config: &UpdateConfig) -> std::io::Result<()> {
    fs::create_dir_all(config_dir())?;
    let body = toml::to_string_pretty(config)
        .unwrap_or_else(|_| String::from("# Hoard update configuration\n"));
    atomic_write(&update_config_path(), body.as_bytes())
}

pub fn load_settings() -> AppSettings {
    let path = settings_path();
    let Ok(body) = fs::read_to_string(&path) else {
        return AppSettings::default();
    };
    match toml::from_str(&body) {
        Ok(settings) => settings,
        Err(_) => {
            quarantine_corrupt(&path);
            AppSettings::default()
        }
    }
}

fn quarantine_corrupt(path: &Path) {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let backup = path.with_extension(format!("corrupt-{stamp}"));
    let _ = fs::rename(path, backup);
}

pub fn save_settings(settings: &AppSettings) -> std::io::Result<()> {
    fs::create_dir_all(config_dir())?;
    let body =
        toml::to_string_pretty(settings).unwrap_or_else(|_| String::from("# Hoard settings\n"));
    atomic_write(&settings_path(), body.as_bytes())
}

fn default_pinned() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    [home.join("Downloads"), home.join("Pictures/Screenshots")]
        .into_iter()
        .filter(|p| p.exists())
        .collect()
}

pub fn load_pinned() -> Vec<PathBuf> {
    if pinned_path().exists() {
        return fs::read_to_string(pinned_path())
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .filter(|p| p.exists())
            .collect();
    }

    let mut paths = default_pinned();
    let legacy = fs::read_to_string(legacy_bookmarks_path()).unwrap_or_default();
    for path in legacy
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .filter(|p| p.exists())
    {
        if !paths.contains(&path) {
            paths.push(path);
        }
    }

    save_pinned(&paths);
    paths
}

pub fn save_pinned(paths: &[PathBuf]) {
    let _ = fs::create_dir_all(config_dir());
    let body = paths
        .iter()
        .map(|p| p.to_string_lossy())
        .collect::<Vec<_>>()
        .join("\n");
    let _ = atomic_write(&pinned_path(), format!("{}\n", body).as_bytes());
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionTab {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub back: Vec<String>,
    #[serde(default)]
    pub forward: Vec<String>,
    #[serde(default)]
    pub search: String,
    #[serde(default)]
    pub selected: Vec<String>,
    #[serde(default)]
    pub view: String,
    #[serde(default)]
    pub scroll_y: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionState {
    // Legacy field from <= dev.7. Kept for a seamless migration.
    #[serde(default)]
    pub tabs: Vec<String>,
    #[serde(default)]
    pub tab_state: Vec<SessionTab>,
    #[serde(default)]
    pub active_tab: usize,
}

pub fn load_session() -> SessionState {
    let path = session_path();
    let Ok(body) = fs::read_to_string(&path) else {
        return SessionState::default();
    };
    match toml::from_str(&body) {
        Ok(session) => session,
        Err(_) => {
            quarantine_corrupt(&path);
            SessionState::default()
        }
    }
}

pub fn save_session(session: &SessionState) -> std::io::Result<()> {
    fs::create_dir_all(config_dir())?;
    let body =
        toml::to_string_pretty(session).unwrap_or_else(|_| String::from("# Hoard session\n"));
    atomic_write(&session_path(), body.as_bytes())
}

pub fn reset_settings() -> std::io::Result<AppSettings> {
    let defaults = AppSettings::default();
    save_settings(&defaults)?;
    Ok(defaults)
}

pub fn network_history_path() -> PathBuf {
    config_dir().join("network-history.txt")
}

pub fn load_network_history() -> Vec<String> {
    fs::read_to_string(network_history_path())
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .take(16)
        .map(ToOwned::to_owned)
        .collect()
}

pub fn save_network_history(items: &[String]) {
    let _ = fs::create_dir_all(config_dir());
    let body = items
        .iter()
        .take(16)
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    let _ = atomic_write(&network_history_path(), format!("{}\n", body).as_bytes());
}
