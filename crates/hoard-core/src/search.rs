use crate::fs_model::FileEntry;
use std::{
    fs,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime},
};
use walkdir::WalkDir;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SearchFilter {
    pub query: String,
    pub type_filter: String,
    pub min_size: Option<u64>,
    pub max_age_days: Option<u64>,
}

impl SearchFilter {
    pub fn matches(&self, path: &Path, meta: &fs::Metadata) -> bool {
        let q = self.query.trim().to_ascii_lowercase();
        if !q.is_empty() {
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if !name.contains(&q) {
                return false;
            }
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let tf = self.type_filter.trim().to_ascii_lowercase();
        if !tf.is_empty() && !ext.contains(&tf) {
            return false;
        }
        if self
            .min_size
            .is_some_and(|min| meta.is_file() && meta.len() < min)
        {
            return false;
        }
        if let Some((days, modified)) = self.max_age_days.zip(meta.modified().ok()) {
            let age = SystemTime::now()
                .duration_since(modified)
                .unwrap_or(Duration::ZERO);
            if age > Duration::from_secs(days.saturating_mul(86_400)) {
                return false;
            }
        }
        true
    }
}

pub fn recursive_search(
    root: &Path,
    filter: &SearchFilter,
    show_hidden: bool,
    max_results: usize,
    cancel: &Arc<AtomicBool>,
) -> Vec<FileEntry> {
    let mut out = Vec::new();
    for item in WalkDir::new(root)
        .min_depth(1)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let path = item.path();
        if !show_hidden
            && path
                .components()
                .any(|c| c.as_os_str().to_string_lossy().starts_with('.'))
        {
            continue;
        }
        let Ok(meta) = fs::symlink_metadata(path) else {
            continue;
        };
        if !filter.matches(path, &meta) {
            continue;
        }
        let is_dir = meta.is_dir();
        let rel = path.strip_prefix(root).unwrap_or(path);
        let name = rel.to_string_lossy().to_string();
        out.push(FileEntry {
            path: PathBuf::from(path),
            name,
            is_dir,
            size: if is_dir { 0 } else { meta.len() },
            modified: meta.modified().ok(),
            created: meta.created().ok(),
            accessed: meta.accessed().ok(),
            owner: Some(meta.uid()),
            mode: Some(meta.mode()),
            file_type: if is_dir {
                "Folder".into()
            } else {
                crate::fs_model::type_description(path)
            },
            metadata_loaded: true,
        });
        if out.len() >= max_results.max(1) {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::{Arc, atomic::AtomicBool},
    };

    #[test]
    fn recursive_search_filters_by_name_and_type() {
        let td = tempfile::tempdir().unwrap();
        fs::create_dir(td.path().join("nested")).unwrap();
        fs::write(td.path().join("nested/report.txt"), b"hello").unwrap();
        fs::write(td.path().join("nested/image.png"), b"not-an-image").unwrap();
        let filter = SearchFilter {
            query: "report".into(),
            type_filter: "txt".into(),
            min_size: None,
            max_age_days: None,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let out = recursive_search(td.path(), &filter, true, 100, &cancel);
        assert_eq!(out.len(), 1);
        assert!(out[0].path.ends_with("report.txt"));
    }
}
