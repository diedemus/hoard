use crate::{config, fs_model::FileEntry};
use serde::{Deserialize, Serialize};
use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Serialize, Deserialize)]
struct CachedDirectory {
    path: String,
    show_hidden: bool,
    signature: Option<(u64, u64)>,
    saved_at: u64,
    entries: Vec<CachedEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CachedEntry {
    path: String,
    name: String,
    is_dir: bool,
    size: u64,
    modified: Option<u64>,
    #[serde(default)]
    created: Option<u64>,
    accessed: Option<u64>,
    #[serde(default)]
    owner: Option<u32>,
    #[serde(default)]
    mode: Option<u32>,
    file_type: String,
    metadata_loaded: bool,
}

fn system_to_secs(t: Option<SystemTime>) -> Option<u64> {
    t.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
}

fn secs_to_system(t: Option<u64>) -> Option<SystemTime> {
    t.map(|s| UNIX_EPOCH + Duration::from_secs(s))
}

impl From<&FileEntry> for CachedEntry {
    fn from(e: &FileEntry) -> Self {
        Self {
            path: e.path.to_string_lossy().to_string(),
            name: e.name.clone(),
            is_dir: e.is_dir,
            size: e.size,
            modified: system_to_secs(e.modified),
            created: system_to_secs(e.created),
            accessed: system_to_secs(e.accessed),
            owner: e.owner,
            mode: e.mode,
            file_type: e.file_type.clone(),
            metadata_loaded: e.metadata_loaded,
        }
    }
}

impl From<CachedEntry> for FileEntry {
    fn from(e: CachedEntry) -> Self {
        Self {
            path: PathBuf::from(e.path),
            name: e.name,
            is_dir: e.is_dir,
            size: e.size,
            modified: secs_to_system(e.modified),
            created: secs_to_system(e.created),
            accessed: secs_to_system(e.accessed),
            owner: e.owner,
            mode: e.mode,
            file_type: e.file_type,
            metadata_loaded: e.metadata_loaded,
        }
    }
}

pub fn directory_signature(path: &Path) -> Option<(u64, u64)> {
    let m = fs::metadata(path).ok()?;
    let modified = m
        .modified()
        .ok()?
        .duration_since(UNIX_EPOCH)
        .ok()?
        .as_nanos() as u64;
    Some((modified, m.len()))
}

fn directory_cache_root() -> PathBuf {
    config::cache_dir().join("directories")
}

fn cache_path(path: &Path, show_hidden: bool) -> PathBuf {
    let mut h = DefaultHasher::new();
    path.hash(&mut h);
    show_hidden.hash(&mut h);
    directory_cache_root().join(format!("{:016x}.json", h.finish()))
}

pub fn load_directory(
    path: &Path,
    show_hidden: bool,
    require_fresh: bool,
) -> Option<Vec<FileEntry>> {
    let data = fs::read(cache_path(path, show_hidden)).ok()?;
    let cached: CachedDirectory = serde_json::from_slice(&data).ok()?;
    if cached.path != path.to_string_lossy() || cached.show_hidden != show_hidden {
        return None;
    }
    if require_fresh && cached.signature != directory_signature(path) {
        return None;
    }
    Some(cached.entries.into_iter().map(FileEntry::from).collect())
}

pub fn store_directory(
    path: &Path,
    show_hidden: bool,
    entries: &[FileEntry],
) -> std::io::Result<()> {
    fs::create_dir_all(directory_cache_root())?;
    let record = CachedDirectory {
        path: path.to_string_lossy().to_string(),
        show_hidden,
        signature: directory_signature(path),
        saved_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
        entries: entries.iter().map(CachedEntry::from).collect(),
    };
    let bytes = serde_json::to_vec(&record).unwrap_or_default();
    fs::write(cache_path(path, show_hidden), bytes)
}

pub fn invalidate_directory(path: &Path) {
    for show_hidden in [false, true] {
        let _ = fs::remove_file(cache_path(path, show_hidden));
    }
    if let Some(parent) = path.parent() {
        for show_hidden in [false, true] {
            let _ = fs::remove_file(cache_path(parent, show_hidden));
        }
    }
}

pub fn clear_directory_cache() -> std::io::Result<()> {
    let root = directory_cache_root();
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    fs::create_dir_all(root)
}

pub fn directory_cache_bytes() -> u64 {
    folder_bytes(&directory_cache_root())
}

pub fn thumbnail_cache_bytes() -> u64 {
    folder_bytes(&config::cache_dir().join("thumbnails"))
}

pub fn clear_thumbnail_cache() -> std::io::Result<()> {
    let root = config::cache_dir().join("thumbnails");
    if root.exists() {
        fs::remove_dir_all(&root)?;
    }
    fs::create_dir_all(root)
}

pub fn prune_thumbnail_cache(limit_mb: u64, max_age_days: u64) -> std::io::Result<()> {
    let root = config::cache_dir().join("thumbnails");
    if !root.exists() {
        return Ok(());
    }
    let limit = limit_mb.saturating_mul(1024 * 1024);
    let mut files = Vec::new();
    let mut total = 0u64;
    for item in fs::read_dir(&root)? {
        let item = item?;
        let p = item.path();
        let Ok(m) = item.metadata() else {
            continue;
        };
        if !m.is_file() {
            continue;
        }
        let modified = m.modified().unwrap_or(UNIX_EPOCH);
        let age = SystemTime::now()
            .duration_since(modified)
            .unwrap_or_default();
        if max_age_days > 0 && age > Duration::from_secs(max_age_days.saturating_mul(86_400)) {
            let _ = fs::remove_file(&p);
            continue;
        }
        total = total.saturating_add(m.len());
        files.push((modified, m.len(), p));
    }
    if total <= limit {
        return Ok(());
    }
    files.sort_by_key(|(m, _, _)| *m);
    for (_, size, p) in files {
        if total <= limit {
            break;
        }
        if fs::remove_file(p).is_ok() {
            total = total.saturating_sub(size);
        }
    }
    Ok(())
}

fn folder_bytes(root: &Path) -> u64 {
    let Ok(rd) = fs::read_dir(root) else {
        return 0;
    };
    rd.flatten()
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fs_model::FileEntry;

    #[test]
    fn cached_entry_round_trip_preserves_identity() {
        let original = FileEntry {
            path: PathBuf::from("/tmp/example.txt"),
            name: "example.txt".into(),
            is_dir: false,
            size: 42,
            modified: None,
            created: None,
            accessed: None,
            owner: None,
            mode: None,
            file_type: "Text File".into(),
            metadata_loaded: true,
        };
        let cached = CachedEntry::from(&original);
        let restored = FileEntry::from(cached);
        assert_eq!(restored.path, original.path);
        assert_eq!(restored.name, original.name);
        assert_eq!(restored.size, 42);
        assert!(restored.metadata_loaded);
    }
}
