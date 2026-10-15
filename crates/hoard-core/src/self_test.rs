use crate::{
    cache, config, fs_model,
    history::{Signature, UndoAction, UndoHistory},
};
use anyhow::{Result, anyhow};
use std::{fs, path::PathBuf};

fn pass(name: &str) {
    println!("PASS  {name}");
}

pub fn run() -> Result<()> {
    println!("Hoard {} self-test", env!("CARGO_PKG_VERSION"));
    let root = config::cache_dir().join(format!("self-test-{}", std::process::id()));
    if root.exists() {
        let _ = fs::remove_dir_all(&root);
    }
    fs::create_dir_all(&root)?;
    let result = run_inner(&root);
    let _ = fs::remove_dir_all(&root);
    result
}

fn run_inner(root: &std::path::Path) -> Result<()> {
    let settings = config::load_settings();
    let serialized = toml::to_string(&settings)?;
    let _: config::AppSettings = toml::from_str(&serialized)?;
    pass("configuration");

    let session = config::SessionState::default();
    let _ = toml::to_string(&session)?;
    pass("session persistence");

    let listing = root.join("listing");
    fs::create_dir(&listing)?;
    fs::write(listing.join("one.txt"), b"one")?;
    let entries = fs_model::read_directory(&listing, true)?;
    cache::store_directory(&listing, true, &entries)?;
    let cached = cache::load_directory(&listing, true, true)
        .ok_or_else(|| anyhow!("directory cache read failed"))?;
    if cached.len() != entries.len() {
        return Err(anyhow!("directory cache count mismatch"));
    }
    pass("directory cache");

    fs::create_dir_all(config::cache_dir().join("thumbnails"))?;
    pass("thumbnail cache");

    let ops = root.join("ops");
    let dest = root.join("dest");
    fs::create_dir(&ops)?;
    fs::create_dir(&dest)?;
    let source = ops.join("copy.txt");
    fs::write(&source, b"copy")?;
    let copied =
        fs_model::copy_item_with_policy(&source, &dest, fs_model::CollisionPolicy::KeepBoth)?
            .ok_or_else(|| anyhow!("copy unexpectedly skipped"))?;
    if !copied.exists() {
        return Err(anyhow!("copy did not create destination"));
    }
    let moved_dir = root.join("moved");
    fs::create_dir(&moved_dir)?;
    let moved =
        fs_model::move_item_with_policy(&copied, &moved_dir, fs_model::CollisionPolicy::KeepBoth)?
            .ok_or_else(|| anyhow!("move unexpectedly skipped"))?;
    if !moved.exists() {
        return Err(anyhow!("move did not create destination"));
    }
    pass("copy/move");

    let trash_source = root.join("trash-me.txt");
    fs::write(&trash_source, b"trash")?;
    let trash_path = fs_model::trash_item(&trash_source)?;
    let restored = fs_model::restore_trashed(&trash_path)?;
    if !restored.exists() {
        return Err(anyhow!("restore did not restore item"));
    }
    pass("trash/restore");

    let before = root.join("before.txt");
    let after = root.join("after.txt");
    fs::write(&after, b"rename")?;
    let mut history = UndoHistory::new(8);
    history.push(UndoAction::Rename {
        from: before.clone(),
        to: after.clone(),
    });
    history.undo()?;
    history.redo()?;
    if !after.exists() {
        return Err(anyhow!("undo/redo verification failed"));
    }
    let created = root.join("created.txt");
    fs::write(&created, b"created")?;
    let sig = Signature::capture(&created).ok_or_else(|| anyhow!("signature failed"))?;
    history.push(UndoAction::Created {
        label: "New File".into(),
        paths: vec![(created, sig)],
    });
    pass("undo/redo");

    let update = config::load_update_config();
    if update.provider.trim().is_empty() {
        return Err(anyhow!("update provider is empty"));
    }
    pass("updater configuration");

    let desktop = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/hoard.desktop.in");
    if !desktop.is_file() {
        return Err(anyhow!("desktop file missing"));
    }
    pass("desktop integration");

    if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        pass("native Wayland environment");
    } else {
        println!("SKIP  native Wayland environment (WAYLAND_DISPLAY not set)");
    }

    println!("Core self-test checks complete");
    Ok(())
}
