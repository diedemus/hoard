use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

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

fn default_provider() -> String { "github".into() }
fn default_repository() -> String { DEFAULT_GITHUB_REPOSITORY.into() }
fn default_channel() -> String { "stable".into() }
fn default_interval() -> u64 { 24 }
fn default_true() -> bool { true }

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
}

fn default_view() -> String { "details".into() }
fn default_thumbnail_size() -> u32 { 112 }
fn default_window_opacity() -> f32 { 1.0 }
fn default_terminal() -> String { "alacritty".into() }
fn default_sort_column() -> String { "name".into() }

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
            confirm_permanent_delete: true,
            terminal: default_terminal(),
            sort_column: default_sort_column(),
            sort_ascending: true,
            remember_tabs: true,
        }
    }
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("hoard")
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

pub fn load_update_config() -> UpdateConfig {
    fs::read_to_string(update_config_path())
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn load_settings() -> AppSettings {
    fs::read_to_string(settings_path())
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_settings(settings: &AppSettings) -> std::io::Result<()> {
    fs::create_dir_all(config_dir())?;
    let body = toml::to_string_pretty(settings)
        .unwrap_or_else(|_| String::from("# Hoard settings\n"));
    fs::write(settings_path(), body)
}

pub fn pinned_path() -> PathBuf { config_dir().join("pinned.txt") }
pub fn legacy_bookmarks_path() -> PathBuf { config_dir().join("bookmarks.txt") }

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
        if !paths.contains(&path) { paths.push(path); }
    }

    save_pinned(&paths);
    paths
}

pub fn save_pinned(paths: &[PathBuf]) {
    let _ = fs::create_dir_all(config_dir());
    let body = paths.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>().join("\n");
    let _ = fs::write(pinned_path(), format!("{}\n", body));
}


#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionState {
    #[serde(default)]
    pub tabs: Vec<String>,
    #[serde(default)]
    pub active_tab: usize,
}

pub fn session_path() -> PathBuf { config_dir().join("session.toml") }

pub fn load_session() -> SessionState {
    fs::read_to_string(session_path())
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save_session(session: &SessionState) -> std::io::Result<()> {
    fs::create_dir_all(config_dir())?;
    let body = toml::to_string_pretty(session)
        .unwrap_or_else(|_| String::from("# Hoard session\n"));
    fs::write(session_path(), body)
}
