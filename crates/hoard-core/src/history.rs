use crate::fs_model;
use anyhow::{Result, anyhow};
use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};
use walkdir::WalkDir;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    pub is_dir: bool,
    pub len: u64,
    pub modified_ns: Option<u128>,
    pub tree_hash: Option<u64>,
}

impl Signature {
    pub fn capture(path: &Path) -> Option<Self> {
        let m = fs::symlink_metadata(path).ok()?;
        let tree_hash = if m.is_dir() {
            let mut h = DefaultHasher::new();
            for entry in WalkDir::new(path)
                .follow_links(false)
                .into_iter()
                .filter_map(|entry| entry.ok())
            {
                let rel = entry.path().strip_prefix(path).unwrap_or(entry.path());
                rel.hash(&mut h);
                if let Ok(meta) = fs::symlink_metadata(entry.path()) {
                    meta.len().hash(&mut h);
                    meta.is_dir().hash(&mut h);
                    if let Some(mt) = meta
                        .modified()
                        .ok()
                        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    {
                        mt.as_nanos().hash(&mut h);
                    }
                }
            }
            Some(h.finish())
        } else {
            None
        };
        Some(Self {
            is_dir: m.is_dir(),
            len: m.len(),
            modified_ns: m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos()),
            tree_hash,
        })
    }

    pub fn unchanged(&self, path: &Path) -> bool {
        Self::capture(path).as_ref() == Some(self)
    }
}

#[derive(Clone, Debug)]
pub enum UndoAction {
    Rename {
        from: PathBuf,
        to: PathBuf,
    },
    Move {
        pairs: Vec<(PathBuf, PathBuf)>,
    },
    Trash {
        pairs: Vec<(PathBuf, PathBuf)>,
    },
    Restore {
        pairs: Vec<(PathBuf, PathBuf, Signature)>,
    },
    Created {
        label: String,
        paths: Vec<(PathBuf, Signature)>,
    },
}

#[derive(Clone, Debug)]
enum RedoAction {
    Rename { from: PathBuf, to: PathBuf },
    Move { pairs: Vec<(PathBuf, PathBuf)> },
    Trash { originals: Vec<PathBuf> },
    Restore { trash_paths: Vec<PathBuf> },
}

fn short_name(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

impl UndoAction {
    pub fn description(&self) -> String {
        match self {
            Self::Rename { from, to } => {
                format!(
                    "Undo Rename \"{}\" → \"{}\"",
                    short_name(from),
                    short_name(to)
                )
            }
            Self::Move { pairs } => match pairs.as_slice() {
                [(from, to)] => format!(
                    "Undo Move \"{}\" → {}",
                    short_name(from),
                    to.parent().unwrap_or(to).display()
                ),
                _ => format!("Undo Move {} items", pairs.len()),
            },
            Self::Trash { pairs } => match pairs.as_slice() {
                [(original, _)] => format!("Undo Trash \"{}\"", short_name(original)),
                _ => format!("Undo Trash {} items", pairs.len()),
            },
            Self::Restore { pairs } => match pairs.as_slice() {
                [(_, restored, _)] => format!("Undo Restore \"{}\"", short_name(restored)),
                _ => format!("Undo Restore {} items", pairs.len()),
            },
            Self::Created { label, paths } => match paths.as_slice() {
                [(path, _)] => format!("Undo {label} \"{}\"", short_name(path)),
                _ => format!("Undo {label} {} items", paths.len()),
            },
        }
    }

    fn undo(self) -> Result<(String, Option<RedoAction>)> {
        match self {
            Self::Rename { from, to } => {
                if !to.exists() {
                    return Err(anyhow!("renamed item no longer exists"));
                }
                if from.exists() {
                    return Err(anyhow!("original name is now occupied"));
                }
                fs::rename(&to, &from)?;
                Ok(("Undid Rename".into(), Some(RedoAction::Rename { from, to })))
            }
            Self::Move { pairs } => {
                for (from, to) in pairs.iter().rev() {
                    if !to.exists() {
                        return Err(anyhow!("moved item no longer exists: {}", to.display()));
                    }
                    if from.exists() {
                        return Err(anyhow!("original location is occupied: {}", from.display()));
                    }
                }
                for (from, to) in pairs.iter().rev() {
                    let parent = from
                        .parent()
                        .ok_or_else(|| anyhow!("invalid original path"))?;
                    fs::create_dir_all(parent)?;
                    fs_model::move_item_exact(to, from)?;
                }
                Ok(("Undid Move".into(), Some(RedoAction::Move { pairs })))
            }
            Self::Trash { pairs } => {
                for (original, trash) in &pairs {
                    if original.exists() {
                        return Err(anyhow!(
                            "original location is occupied: {}",
                            original.display()
                        ));
                    }
                    if !trash.exists() {
                        return Err(anyhow!("trash item no longer exists: {}", trash.display()));
                    }
                }
                let mut restored = Vec::new();
                for (_, trash) in &pairs {
                    restored.push(fs_model::restore_trashed(trash)?);
                }
                Ok((
                    "Undid Move to Trash".into(),
                    Some(RedoAction::Trash {
                        originals: restored,
                    }),
                ))
            }
            Self::Restore { pairs } => {
                for (_, restored, sig) in &pairs {
                    if !sig.unchanged(restored) {
                        return Err(anyhow!(
                            "restored item changed; refusing unsafe undo: {}",
                            restored.display()
                        ));
                    }
                }
                let mut trash_paths = Vec::new();
                for (_, restored, _) in &pairs {
                    trash_paths.push(fs_model::trash_item(restored)?);
                }
                Ok((
                    "Undid Restore".into(),
                    Some(RedoAction::Restore { trash_paths }),
                ))
            }
            Self::Created { label, paths } => {
                for (path, sig) in &paths {
                    if !sig.unchanged(path) {
                        return Err(anyhow!("{} changed; refusing unsafe undo", path.display()));
                    }
                }
                for (path, _) in paths.into_iter().rev() {
                    fs_model::delete_permanently(&path)?;
                }
                Ok((format!("Undid {label}"), None))
            }
        }
    }
}

impl RedoAction {
    fn description(&self) -> String {
        match self {
            Self::Rename { from, to } => {
                format!(
                    "Redo Rename \"{}\" → \"{}\"",
                    short_name(from),
                    short_name(to)
                )
            }
            Self::Move { pairs } => format!("Redo Move {} item(s)", pairs.len()),
            Self::Trash { originals } => format!("Redo Trash {} item(s)", originals.len()),
            Self::Restore { trash_paths } => format!("Redo Restore {} item(s)", trash_paths.len()),
        }
    }

    fn redo(self) -> Result<(String, UndoAction)> {
        match self {
            Self::Rename { from, to } => {
                if !from.exists() {
                    return Err(anyhow!("item to redo no longer exists"));
                }
                if to.exists() {
                    return Err(anyhow!("redo destination is occupied"));
                }
                fs::rename(&from, &to)?;
                Ok(("Redid Rename".into(), UndoAction::Rename { from, to }))
            }
            Self::Move { pairs } => {
                for (from, to) in &pairs {
                    if !from.exists() {
                        return Err(anyhow!("item to redo no longer exists: {}", from.display()));
                    }
                    if to.exists() {
                        return Err(anyhow!("redo destination is occupied: {}", to.display()));
                    }
                }
                for (from, to) in &pairs {
                    fs_model::move_item_exact(from, to)?;
                }
                Ok(("Redid Move".into(), UndoAction::Move { pairs }))
            }
            Self::Trash { originals } => {
                let mut pairs = Vec::new();
                for original in originals {
                    if !original.exists() {
                        return Err(anyhow!(
                            "item to trash no longer exists: {}",
                            original.display()
                        ));
                    }
                    let trash = fs_model::trash_item(&original)?;
                    pairs.push((original, trash));
                }
                Ok(("Redid Move to Trash".into(), UndoAction::Trash { pairs }))
            }
            Self::Restore { trash_paths } => {
                let mut pairs = Vec::new();
                for trash in trash_paths {
                    if !trash.exists() {
                        return Err(anyhow!("trash item no longer exists: {}", trash.display()));
                    }
                    let restored = fs_model::restore_trashed(&trash)?;
                    let sig = Signature::capture(&restored)
                        .ok_or_else(|| anyhow!("could not verify restored item"))?;
                    pairs.push((trash, restored, sig));
                }
                Ok(("Redid Restore".into(), UndoAction::Restore { pairs }))
            }
        }
    }
}

#[derive(Default)]
pub struct UndoHistory {
    undo: Vec<UndoAction>,
    redo: Vec<RedoAction>,
    max_entries: usize,
}

impl UndoHistory {
    pub fn new(max_entries: usize) -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            max_entries: max_entries.max(1),
        }
    }

    pub fn push(&mut self, action: UndoAction) {
        self.undo.push(action);
        self.redo.clear();
        if self.undo.len() > self.max_entries {
            self.undo.remove(0);
        }
    }

    pub fn next_label(&self) -> Option<String> {
        self.undo.last().map(UndoAction::description)
    }

    pub fn next_redo_label(&self) -> Option<String> {
        self.redo.last().map(RedoAction::description)
    }

    pub fn undo(&mut self) -> Result<String> {
        let action = self.undo.pop().ok_or_else(|| anyhow!("Nothing to undo"))?;
        match action.clone().undo() {
            Ok((message, redo)) => {
                if let Some(redo) = redo {
                    self.redo.push(redo);
                } else {
                    self.redo.clear();
                }
                Ok(message)
            }
            Err(error) => {
                self.undo.push(action);
                Err(error)
            }
        }
    }

    pub fn redo(&mut self) -> Result<String> {
        let action = self.redo.pop().ok_or_else(|| anyhow!("Nothing to redo"))?;
        match action.clone().redo() {
            Ok((message, undo)) => {
                self.undo.push(undo);
                Ok(message)
            }
            Err(error) => {
                self.redo.push(action);
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn created_file_undo_refuses_modified_content() {
        let td = tempfile::tempdir().unwrap();
        let p = td.path().join("created.txt");
        fs::write(&p, b"one").unwrap();
        let sig = Signature::capture(&p).unwrap();
        fs::write(&p, b"changed-content").unwrap();
        let action = UndoAction::Created {
            label: "New File".into(),
            paths: vec![(p.clone(), sig)],
        };
        assert!(action.undo().is_err());
        assert!(p.exists());
    }

    #[test]
    fn rename_supports_undo_and_redo() {
        let td = tempfile::tempdir().unwrap();
        let from = td.path().join("before.txt");
        let to = td.path().join("after.txt");
        fs::write(&to, b"data").unwrap();
        let mut history = UndoHistory::new(8);
        history.push(UndoAction::Rename {
            from: from.clone(),
            to: to.clone(),
        });
        history.undo().unwrap();
        assert!(from.exists());
        assert!(!to.exists());
        history.redo().unwrap();
        assert!(!from.exists());
        assert!(to.exists());
    }
}
