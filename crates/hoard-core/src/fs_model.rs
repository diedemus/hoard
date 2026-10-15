use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, Local};
use nix::sys::statvfs::statvfs;
use percent_encoding::{NON_ALPHANUMERIC, percent_decode, percent_encode};
use std::{
    ffi::{OsStr, OsString},
    fs,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::SystemTime,
};
use walkdir::WalkDir;

#[derive(Clone, Debug)]
pub struct FileEntry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
    pub owner: Option<u32>,
    pub mode: Option<u32>,
    pub file_type: String,
    pub metadata_loaded: bool,
}

#[derive(Clone, Debug)]
pub struct EntryMetadata {
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub created: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
    pub owner: Option<u32>,
    pub mode: Option<u32>,
}

#[derive(Clone, Debug)]
pub struct FolderStats {
    pub file_count: u64,
    pub folder_count: u64,
    pub apparent_size: u64,
    pub allocated_size: u64,
    pub free_space: u64,
    pub total_space: u64,
    pub modified: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortColumn {
    Name,
    Type,
    Size,
    Modified,
    Created,
    Owner,
    Permissions,
}

pub fn read_directory(path: &Path, show_hidden: bool) -> std::io::Result<Vec<FileEntry>> {
    // Keep the first directory pass deliberately cheap. On large image folders,
    // especially removable disks, a metadata stat for every item can dominate
    // startup time. Size/timestamps are hydrated asynchronously by the UI only
    // when a details-oriented view actually needs them. The read itself returns
    // an error so path validation can remain off the UI thread.
    let mut out = Vec::new();
    let rd = fs::read_dir(path)?;

    for item in rd.flatten() {
        let p = item.path();
        let name = item.file_name().to_string_lossy().to_string();
        if !show_hidden && name.starts_with('.') {
            continue;
        }

        let is_dir = item
            .file_type()
            .map(|t| t.is_dir())
            .unwrap_or_else(|_| p.is_dir());
        let file_type = if is_dir {
            "Folder".to_string()
        } else {
            p.extension()
                .and_then(|e| e.to_str())
                .map(type_for_extension)
                .unwrap_or_else(|| "File".into())
        };

        out.push(FileEntry {
            path: p,
            name,
            is_dir,
            size: 0,
            modified: None,
            created: None,
            accessed: None,
            owner: None,
            mode: None,
            file_type,
            metadata_loaded: false,
        });
    }
    Ok(out)
}

pub fn load_metadata(paths: &[PathBuf]) -> Vec<EntryMetadata> {
    paths
        .iter()
        .filter_map(|path| {
            let meta = fs::metadata(path).ok()?;
            Some(EntryMetadata {
                path: path.clone(),
                size: if meta.is_dir() { 0 } else { meta.len() },
                modified: meta.modified().ok(),
                created: meta.created().ok(),
                accessed: meta.accessed().ok(),
                owner: Some(meta.uid()),
                mode: Some(meta.mode()),
            })
        })
        .collect()
}

pub fn type_for_extension(ext: &str) -> String {
    match ext.to_ascii_lowercase().as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tif" | "tiff" | "svg" => "Image".into(),
        "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" | "mpeg" | "mpg" => "Video".into(),
        "mp3" | "flac" | "ogg" | "wav" | "m4a" | "aac" | "opus" => "Audio".into(),
        "pdf" => "PDF Document".into(),
        "stl" | "obj" | "3mf" | "step" | "stp" | "ply" | "glb" | "gltf" => "3D Model".into(),
        "zip" | "gz" | "xz" | "bz2" | "zst" | "7z" | "rar" | "tar" | "tgz" => "Archive".into(),
        "doc" | "docx" | "odt" | "rtf" => "Document".into(),
        "xls" | "xlsx" | "ods" | "csv" => "Spreadsheet".into(),
        "ppt" | "pptx" | "odp" => "Presentation".into(),
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "c" | "cc" | "cpp" | "h" | "hpp" | "go"
        | "java" | "kt" | "lua" | "sh" | "bash" | "zsh" | "ps1" => "Source Code".into(),
        "toml" | "json" | "yaml" | "yml" | "ini" | "conf" | "cfg" | "xml" => "Configuration".into(),
        "txt" | "md" | "log" => "Text File".into(),
        "ttf" | "otf" | "woff" | "woff2" => "Font".into(),
        "iso" | "img" => "Disk Image".into(),
        "appimage" | "exe" | "msi" | "run" => "Application".into(),
        "" => "File".into(),
        other => format!("{} File", other.to_ascii_uppercase()),
    }
}

pub fn type_description(path: &Path) -> String {
    if path.is_dir() {
        return "Folder".into();
    }
    path.extension()
        .and_then(|e| e.to_str())
        .map(type_for_extension)
        .unwrap_or_else(|| "File".into())
}

pub fn is_image_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tif" | "tiff"
            )
        })
        .unwrap_or(false)
}

pub fn is_visual_media_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "png"
                    | "jpg"
                    | "jpeg"
                    | "webp"
                    | "gif"
                    | "bmp"
                    | "tif"
                    | "tiff"
                    | "svg"
                    | "mp4"
                    | "mkv"
                    | "webm"
                    | "mov"
                    | "avi"
                    | "m4v"
                    | "mpeg"
                    | "mpg"
                    | "pdf"
                    | "stl"
                    | "obj"
                    | "3mf"
                    | "step"
                    | "stp"
                    | "ply"
                    | "glb"
                    | "gltf"
            )
        })
        .unwrap_or(false)
}

pub fn icon_badge(entry: &FileEntry) -> &'static str {
    if entry.is_dir {
        return "";
    }
    match entry.file_type.as_str() {
        "Image" => "IMG",
        "Video" => "VID",
        "Audio" => "AUD",
        "PDF Document" => "PDF",
        "3D Model" => "3D",
        "Archive" => "ZIP",
        "Document" => "DOC",
        "Spreadsheet" => "XLS",
        "Presentation" => "PPT",
        "Source Code" => "{ }",
        "Configuration" => "CFG",
        "Text File" => "TXT",
        "Font" => "Aa",
        "Disk Image" => "ISO",
        "Application" => "APP",
        _ => "FILE",
    }
}

pub fn sort_entries(entries: &mut [FileEntry], col: SortColumn, ascending: bool) {
    entries.sort_by(|a, b| {
        if a.is_dir != b.is_dir {
            return b.is_dir.cmp(&a.is_dir);
        }
        let ord = match col {
            SortColumn::Name => a
                .name
                .to_ascii_lowercase()
                .cmp(&b.name.to_ascii_lowercase()),
            SortColumn::Type => a
                .file_type
                .cmp(&b.file_type)
                .then_with(|| a.name.cmp(&b.name)),
            SortColumn::Size => a.size.cmp(&b.size).then_with(|| a.name.cmp(&b.name)),
            SortColumn::Modified => a
                .modified
                .cmp(&b.modified)
                .then_with(|| a.name.cmp(&b.name)),
            SortColumn::Created => a.created.cmp(&b.created).then_with(|| a.name.cmp(&b.name)),
            SortColumn::Owner => a.owner.cmp(&b.owner).then_with(|| a.name.cmp(&b.name)),
            SortColumn::Permissions => a
                .mode
                .map(|m| m & 0o7777)
                .cmp(&b.mode.map(|m| m & 0o7777))
                .then_with(|| a.name.cmp(&b.name)),
        };
        if ascending { ord } else { ord.reverse() }
    });
}

pub fn format_size(bytes: u64) -> String {
    if bytes == 0 {
        return "—".into();
    }
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut n = bytes as f64;
    let mut i = 0usize;
    while n >= 1024.0 && i < UNITS.len() - 1 {
        n /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", bytes, UNITS[i])
    } else {
        format!("{:.1} {}", n, UNITS[i])
    }
}

pub fn format_modified(t: Option<SystemTime>) -> String {
    t.map(|x| {
        let dt: DateTime<Local> = x.into();
        dt.format("%b %d, %Y %H:%M").to_string()
    })
    .unwrap_or_else(|| "—".into())
}

pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    let s = statvfs(path).ok()?;
    let block = s.block_size();
    Some((s.blocks_available() * block, s.blocks() * block))
}

pub fn folder_stats(path: &Path, cancel: &AtomicBool) -> Option<FolderStats> {
    let root_meta = fs::metadata(path).ok()?;
    if !root_meta.is_dir() {
        return None;
    }

    let mut file_count = 0_u64;
    let mut folder_count = 0_u64;
    let mut apparent_size = 0_u64;
    let mut allocated_size = root_meta.blocks().saturating_mul(512);

    for entry in WalkDir::new(path)
        .follow_links(false)
        .min_depth(1)
        .into_iter()
        .flatten()
    {
        if cancel.load(Ordering::Relaxed) {
            return None;
        }
        let Ok(meta) = entry.metadata() else { continue };
        allocated_size = allocated_size.saturating_add(meta.blocks().saturating_mul(512));
        if meta.is_dir() {
            folder_count = folder_count.saturating_add(1);
        } else if meta.is_file() {
            file_count = file_count.saturating_add(1);
            apparent_size = apparent_size.saturating_add(meta.len());
        }
    }

    let (free_space, total_space) = disk_space(path).unwrap_or((0, 0));
    Some(FolderStats {
        file_count,
        folder_count,
        apparent_size,
        allocated_size,
        free_space,
        total_space,
        modified: root_meta.modified().ok(),
        accessed: root_meta.accessed().ok(),
    })
}

pub fn discover_mounts() -> Vec<(String, PathBuf)> {
    let text = fs::read_to_string("/proc/mounts").unwrap_or_default();
    let mut out = Vec::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let dev = fields.next().unwrap_or("");
        let mount = fields.next().unwrap_or("");
        if mount == "/" || !mount.starts_with('/') {
            continue;
        }
        if !(dev.starts_with("/dev/")
            || mount.starts_with("/run/media/")
            || mount.starts_with("/mnt/"))
        {
            continue;
        }
        let p = PathBuf::from(mount.replace("\\040", " "));
        if !p.exists() {
            continue;
        }
        let label = p
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or(mount)
            .to_string();
        if !out.iter().any(|(_, x)| x == &p) {
            out.push((label, p));
        }
    }
    out
}

pub fn trash_root() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".local/share")
        })
        .join("Trash")
}

pub fn trash_files_dir() -> PathBuf {
    trash_root().join("files")
}
pub fn trash_info_dir() -> PathBuf {
    trash_root().join("info")
}

fn ensure_trash_root(root: &Path) -> Result<()> {
    fs::create_dir_all(root.join("files"))?;
    fs::create_dir_all(root.join("info"))?;
    let private = fs::Permissions::from_mode(0o700);
    let _ = fs::set_permissions(root, private.clone());
    let _ = fs::set_permissions(root.join("files"), private.clone());
    let _ = fs::set_permissions(root.join("info"), private);
    Ok(())
}

fn topdir_for_trash_root(root: &Path) -> Option<&Path> {
    if root == trash_root() {
        return None;
    }
    if root
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.starts_with(".Trash-"))
        .unwrap_or(false)
    {
        return root.parent();
    }
    if root.parent().and_then(|p| p.file_name()) == Some(OsStr::new(".Trash")) {
        return root.parent().and_then(Path::parent);
    }
    None
}

pub fn ensure_trash_dirs() {
    let _ = ensure_trash_root(&trash_root());
}

fn mount_root_for(path: &Path) -> Option<PathBuf> {
    let canonical = path
        .canonicalize()
        .ok()
        .or_else(|| path.parent()?.canonicalize().ok())?;
    let dev = fs::symlink_metadata(&canonical).ok()?.dev();
    let mut cur = canonical.as_path();
    while let Some(parent) = cur.parent() {
        let Ok(meta) = fs::symlink_metadata(parent) else {
            break;
        };
        if meta.dev() != dev {
            break;
        }
        cur = parent;
    }
    Some(cur.to_path_buf())
}

fn trash_root_for(path: &Path) -> Result<PathBuf> {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let home_dev = fs::symlink_metadata(&home).ok().map(|m| m.dev());
    let path_dev = fs::symlink_metadata(path)
        .ok()
        .or_else(|| path.parent().and_then(|p| fs::symlink_metadata(p).ok()))
        .map(|m| m.dev());
    if home_dev.is_some() && home_dev == path_dev {
        return Ok(trash_root());
    }

    let uid = unsafe { nix::libc::geteuid() };
    if let Some(top) = mount_root_for(path) {
        let shared = top.join(".Trash");
        let shared_is_sticky = shared.is_dir()
            && fs::symlink_metadata(&shared)
                .is_ok_and(|meta| meta.permissions().mode() & 0o1000 != 0);
        if shared_is_sticky {
            let user_root = shared.join(uid.to_string());
            if ensure_trash_root(&user_root).is_ok() {
                return Ok(user_root);
            }
        }
        let private = top.join(format!(".Trash-{uid}"));
        if ensure_trash_root(&private).is_ok() {
            return Ok(private);
        }
    }
    Err(anyhow!(
        "this filesystem does not provide a writable freedesktop Trash location"
    ))
}

fn info_path_for_trash_path(path: &Path) -> Result<PathBuf> {
    let files = path.parent().ok_or_else(|| anyhow!("invalid trash path"))?;
    if files.file_name() != Some(OsStr::new("files")) {
        return Err(anyhow!(
            "item is not in a freedesktop Trash files directory"
        ));
    }
    let root = files
        .parent()
        .ok_or_else(|| anyhow!("invalid trash root"))?;
    let mut info_name = path
        .file_name()
        .ok_or_else(|| anyhow!("invalid trash item"))?
        .to_os_string();
    info_name.push(".trashinfo");
    Ok(root.join("info").join(info_name))
}

pub fn trash_count() -> usize {
    fs::read_dir(trash_files_dir())
        .map(|rd| rd.flatten().count())
        .unwrap_or(0)
}

pub fn is_in_trash(path: &Path) -> bool {
    info_path_for_trash_path(path)
        .map(|p| p.exists())
        .unwrap_or(false)
}

pub fn trash_item(path: &Path) -> Result<PathBuf> {
    let root = trash_root_for(path)?;
    ensure_trash_root(&root)?;
    let original = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let name = path
        .file_name()
        .ok_or_else(|| anyhow!("invalid item name"))?;
    let dest = unique_destination_os(&root.join("files"), name, OsStr::new(""));
    move_item_to(path, &dest)?;

    let mut info_name = dest
        .file_name()
        .ok_or_else(|| anyhow!("invalid trash item"))?
        .to_os_string();
    info_name.push(".trashinfo");
    let info_path = root.join("info").join(info_name);
    let stored_path = topdir_for_trash_root(&root)
        .and_then(|top| original.strip_prefix(top).ok())
        .unwrap_or(&original);
    let encoded = percent_encode(stored_path.as_os_str().as_bytes(), NON_ALPHANUMERIC).to_string();
    let now = Local::now().format("%Y-%m-%dT%H:%M:%S");
    let body = format!("[Trash Info]\nPath={encoded}\nDeletionDate={now}\n");
    if let Err(e) = fs::write(&info_path, body) {
        let _ = move_item_to(&dest, path);
        return Err(e.into());
    }
    Ok(dest)
}

pub fn restore_trashed(path: &Path) -> Result<PathBuf> {
    let info = info_path_for_trash_path(path)?;
    let body = fs::read(&info)
        .with_context(|| format!("missing trash metadata for {}", path.display()))?;
    let encoded = body
        .split(|b| *b == b'\n')
        .find_map(|line| line.strip_prefix(b"Path="))
        .ok_or_else(|| anyhow!("trash metadata has no original path"))?;
    let decoded = percent_decode(encoded).collect::<Vec<u8>>();
    let decoded_path = PathBuf::from(OsString::from_vec(decoded));
    let root = info
        .parent()
        .and_then(Path::parent)
        .ok_or_else(|| anyhow!("invalid trash metadata location"))?;
    let requested = if decoded_path.is_absolute() {
        decoded_path
    } else {
        let top = topdir_for_trash_root(root)
            .ok_or_else(|| anyhow!("relative trash path has no filesystem topdir"))?;
        top.join(decoded_path)
    };
    let parent = requested
        .parent()
        .ok_or_else(|| anyhow!("invalid original path"))?;
    fs::create_dir_all(parent)?;
    let dest = if requested.exists() {
        unique_destination_os(
            parent,
            requested
                .file_name()
                .unwrap_or_else(|| path.file_name().unwrap_or(OsStr::new("restored"))),
            OsStr::new(" restored"),
        )
    } else {
        requested
    };
    move_item_to(path, &dest)?;
    let _ = fs::remove_file(info);
    Ok(dest)
}

pub fn empty_trash() -> Result<()> {
    let gio = Command::new("gio")
        .args(["trash", "--empty"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if gio.is_ok_and(|status| status.success()) {
        return Ok(());
    }

    ensure_trash_dirs();
    remove_contents(&trash_files_dir())?;
    remove_contents(&trash_info_dir())?;
    Ok(())
}

fn remove_contents(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for e in fs::read_dir(dir)? {
        let p = e?.path();
        delete_permanently(&p)?;
    }
    Ok(())
}

pub fn delete_permanently(path: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() || meta.is_file() {
        fs::remove_file(path)?;
    } else if meta.is_dir() {
        fs::remove_dir_all(path)?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CollisionPolicy {
    KeepBoth,
    Skip,
    Replace,
}

pub fn copy_item_with_policy(
    src: &Path,
    dest_dir: &Path,
    policy: CollisionPolicy,
) -> Result<Option<PathBuf>> {
    copy_item_with_policy_cancel(src, dest_dir, policy, None)
}

pub fn copy_item_with_policy_cancel(
    src: &Path,
    dest_dir: &Path,
    policy: CollisionPolicy,
    cancel: Option<&Arc<AtomicBool>>,
) -> Result<Option<PathBuf>> {
    if src.is_dir() && dest_dir.starts_with(src) {
        return Err(anyhow!("cannot copy a folder into itself"));
    }
    let name = src
        .file_name()
        .ok_or_else(|| anyhow!("invalid source name"))?;
    let direct = dest_dir.join(name);
    if direct.exists() && policy == CollisionPolicy::Replace {
        let backup = replacement_backup_path(&direct);
        fs::rename(&direct, &backup)?;
        match copy_item_to_cancel(src, &direct, cancel) {
            Ok(()) => {
                let _ = delete_permanently(&backup);
                return Ok(Some(direct));
            }
            Err(error) => {
                let _ = delete_permanently(&direct);
                let _ = fs::rename(&backup, &direct);
                return Err(error);
            }
        }
    }
    let dest = if direct.exists() {
        match policy {
            CollisionPolicy::Skip => return Ok(None),
            CollisionPolicy::KeepBoth => unique_destination_os(dest_dir, name, OsStr::new(" copy")),
            CollisionPolicy::Replace => unreachable!(),
        }
    } else {
        direct
    };
    if let Err(e) = copy_item_to_cancel(src, &dest, cancel) {
        let _ = delete_permanently(&dest);
        return Err(e);
    }
    Ok(Some(dest))
}

pub fn move_item_with_policy(
    src: &Path,
    dest_dir: &Path,
    policy: CollisionPolicy,
) -> Result<Option<PathBuf>> {
    move_item_with_policy_cancel(src, dest_dir, policy, None)
}

pub fn move_item_with_policy_cancel(
    src: &Path,
    dest_dir: &Path,
    policy: CollisionPolicy,
    cancel: Option<&Arc<AtomicBool>>,
) -> Result<Option<PathBuf>> {
    if src.parent() == Some(dest_dir) {
        return Ok(Some(src.to_path_buf()));
    }
    if src.is_dir() && dest_dir.starts_with(src) {
        return Err(anyhow!("cannot move a folder into itself"));
    }
    let name = src
        .file_name()
        .ok_or_else(|| anyhow!("invalid source name"))?;
    let direct = dest_dir.join(name);
    if direct.exists() && policy == CollisionPolicy::Replace {
        let backup = replacement_backup_path(&direct);
        fs::rename(&direct, &backup)?;
        match move_item_to_cancel(src, &direct, cancel) {
            Ok(()) => {
                let _ = delete_permanently(&backup);
                return Ok(Some(direct));
            }
            Err(error) => {
                let _ = delete_permanently(&direct);
                let _ = fs::rename(&backup, &direct);
                return Err(error);
            }
        }
    }
    let dest = if direct.exists() {
        match policy {
            CollisionPolicy::Skip => return Ok(None),
            CollisionPolicy::KeepBoth => unique_destination_os(dest_dir, name, OsStr::new("")),
            CollisionPolicy::Replace => unreachable!(),
        }
    } else {
        direct
    };
    move_item_to_cancel(src, &dest, cancel)?;
    Ok(Some(dest))
}

pub fn move_item_exact(src: &Path, dest: &Path) -> Result<()> {
    if dest.exists() {
        return Err(anyhow!("destination already exists: {}", dest.display()));
    }
    move_item_to(src, dest)
}

pub fn direct_destination(src: &Path, dest_dir: &Path) -> Option<PathBuf> {
    src.file_name().map(|n| dest_dir.join(n))
}

pub fn copy_item(src: &Path, dest_dir: &Path) -> Result<PathBuf> {
    if src.is_dir() && dest_dir.starts_with(src) {
        return Err(anyhow!("cannot copy a folder into itself"));
    }
    let name = src
        .file_name()
        .ok_or_else(|| anyhow!("invalid source name"))?;
    let dest = unique_destination_os(dest_dir, name, OsStr::new(" copy"));
    copy_item_to_cancel(src, &dest, None)?;
    Ok(dest)
}

pub fn duplicate_item(src: &Path) -> Result<PathBuf> {
    let parent = src.parent().ok_or_else(|| anyhow!("item has no parent"))?;
    copy_item(src, parent)
}

pub fn rename_item(src: &Path, new_name: &str) -> Result<PathBuf> {
    let trimmed = new_name.trim();
    if trimmed.is_empty() || trimmed.contains('/') {
        return Err(anyhow!("invalid name"));
    }
    let parent = src.parent().ok_or_else(|| anyhow!("item has no parent"))?;
    let dest = parent.join(trimmed);
    if dest.exists() {
        return Err(anyhow!("an item with that name already exists"));
    }
    fs::rename(src, &dest)?;
    Ok(dest)
}

pub fn create_folder(parent: &Path, name: &str) -> Result<PathBuf> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.contains('/') {
        return Err(anyhow!("invalid folder name"));
    }
    let p = parent.join(trimmed);
    if p.exists() {
        return Err(anyhow!("an item with that name already exists"));
    }
    fs::create_dir(&p)?;
    Ok(p)
}

pub fn create_text_file(parent: &Path, name: &str) -> Result<PathBuf> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.contains('/') {
        return Err(anyhow!("invalid file name"));
    }
    let p = parent.join(trimmed);
    if p.exists() {
        return Err(anyhow!("an item with that name already exists"));
    }
    fs::File::create(&p)?;
    Ok(p)
}

fn cancelled(cancel: Option<&Arc<AtomicBool>>) -> bool {
    cancel.map(|c| c.load(Ordering::Relaxed)).unwrap_or(false)
}

fn preserve_metadata(src: &Path, dest: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(src)?;
    if !meta.file_type().is_symlink() {
        fs::set_permissions(dest, meta.permissions())?;
        if let (Ok(atime), Ok(mtime)) = (meta.accessed(), meta.modified()) {
            let at = filetime::FileTime::from_system_time(atime);
            let mt = filetime::FileTime::from_system_time(mtime);
            let _ = filetime::set_file_times(dest, at, mt);
        }
    }
    Ok(())
}

fn copy_item_to_cancel(src: &Path, dest: &Path, cancel: Option<&Arc<AtomicBool>>) -> Result<()> {
    if cancelled(cancel) {
        return Err(anyhow!("operation cancelled"));
    }
    let meta = fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(fs::read_link(src)?, dest)?;
        }
    } else if meta.is_dir() {
        fs::create_dir(dest)?;
        for entry in WalkDir::new(src).min_depth(1).follow_links(false) {
            if cancelled(cancel) {
                return Err(anyhow!("operation cancelled"));
            }
            let entry = entry?;
            let rel = entry.path().strip_prefix(src)?;
            let target = dest.join(rel);
            if entry.file_type().is_dir() {
                fs::create_dir(&target)?;
            } else if entry.file_type().is_symlink() {
                #[cfg(unix)]
                {
                    std::os::unix::fs::symlink(fs::read_link(entry.path())?, &target)?;
                }
            } else {
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(entry.path(), &target)?;
                preserve_metadata(entry.path(), &target)?;
            }
        }
        for entry in WalkDir::new(src)
            .contents_first(true)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
        {
            let rel = entry.path().strip_prefix(src).unwrap_or(entry.path());
            let target = dest.join(rel);
            if entry.file_type().is_dir() {
                let _ = preserve_metadata(entry.path(), &target);
            }
        }
        preserve_metadata(src, dest)?;
    } else {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(src, dest)?;
        preserve_metadata(src, dest)?;
    }
    Ok(())
}

fn move_item_to(src: &Path, dest: &Path) -> Result<()> {
    move_item_to_cancel(src, dest, None)
}

fn move_item_to_cancel(src: &Path, dest: &Path, cancel: Option<&Arc<AtomicBool>>) -> Result<()> {
    if cancelled(cancel) {
        return Err(anyhow!("operation cancelled"));
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    match fs::rename(src, dest) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(nix::libc::EXDEV) => {
            if let Err(copy_error) = copy_item_to_cancel(src, dest, cancel) {
                let _ = delete_permanently(dest);
                return Err(copy_error);
            }
            if cancelled(cancel) {
                let _ = delete_permanently(dest);
                return Err(anyhow!("operation cancelled"));
            }
            delete_permanently(src)?;
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

fn replacement_backup_path(dest: &Path) -> PathBuf {
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    let mut name = OsString::from(".hoard-replace-");
    name.push(std::process::id().to_string());
    if let Some(original) = dest.file_name() {
        name.push("-");
        name.push(original);
    }
    unique_destination_os(parent, &name, OsStr::new(""))
}

fn unique_destination_os(dir: &Path, original_name: &OsStr, tag: &OsStr) -> PathBuf {
    let direct = dir.join(original_name);
    if !direct.exists() {
        return direct;
    }
    let p = Path::new(original_name);
    let stem = p.file_stem().unwrap_or(original_name);
    let ext = p.extension();
    for i in 1..10_000u32 {
        let mut name = stem.to_os_string();
        name.push(tag);
        if i > 1 {
            name.push(format!(" {i}"));
        }
        if let Some(ext) = ext.filter(|ext| !ext.is_empty()) {
            name.push(".");
            name.push(ext);
        }
        let candidate = dir.join(name);
        if !candidate.exists() {
            return candidate;
        }
    }
    let mut fallback = original_name.to_os_string();
    fallback.push(".hoard-copy");
    dir.join(fallback)
}

pub fn is_remote_path(path: &Path) -> bool {
    let text = fs::read_to_string("/proc/mounts").unwrap_or_default();
    let mut best: Option<(usize, String)> = None;
    for line in text.lines() {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 3 {
            continue;
        }
        let mount = PathBuf::from(fields[1].replace("\\040", " "));
        if path.starts_with(&mount) {
            let len = mount.as_os_str().as_bytes().len();
            if best.as_ref().map(|(n, _)| len > *n).unwrap_or(true) {
                best = Some((len, fields[2].to_string()));
            }
        }
    }
    best.map(|(_, fs_type)| {
        matches!(
            fs_type.as_str(),
            "nfs" | "nfs4" | "cifs" | "smb3" | "sshfs" | "fuse.sshfs" | "davfs" | "9p"
        )
    })
    .unwrap_or(false)
}

fn user_removable_mount(path: &Path) -> bool {
    let s = path.to_string_lossy();
    s.starts_with("/run/media/") || s.starts_with("/media/") || s.starts_with("/mnt/")
}

pub fn unmount_path(path: &Path) -> Result<()> {
    if !user_removable_mount(path) {
        return Err(anyhow!("refusing to unmount a system/non-removable mount"));
    }
    let output = Command::new("udisksctl")
        .args(["unmount", "-b"])
        .arg(device_for_mount(path).ok_or_else(|| anyhow!("could not resolve block device"))?)
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "unmount failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

pub fn power_off_path(path: &Path) -> Result<()> {
    if !user_removable_mount(path) {
        return Err(anyhow!(
            "refusing to power off a system/non-removable mount"
        ));
    }
    let output = Command::new("udisksctl")
        .args(["power-off", "-b"])
        .arg(device_for_mount(path).ok_or_else(|| anyhow!("could not resolve block device"))?)
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "power-off failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

fn device_for_mount(path: &Path) -> Option<String> {
    let text = fs::read_to_string("/proc/mounts").ok()?;
    let target = path.to_string_lossy();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let dev = fields.next()?;
        let mount = fields.next()?.replace("\\040", " ");
        if mount == target && dev.starts_with("/dev/") {
            return Some(dev.to_string());
        }
    }
    None
}

pub fn discover_unmounted_devices() -> Vec<(String, PathBuf)> {
    let output = Command::new("lsblk")
        .args(["-J", "-o", "PATH,LABEL,TYPE,MOUNTPOINT,RM"])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return Vec::new();
    };
    fn walk(node: &serde_json::Value, out: &mut Vec<(String, PathBuf)>) {
        let typ = node.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let path = node.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let mount = node.get("mountpoint");
        let mounted = mount
            .and_then(|v| v.as_str())
            .map(|s| !s.is_empty())
            .unwrap_or(false);
        if !mounted && matches!(typ, "part" | "crypt") && !path.is_empty() {
            let label = node
                .get("label")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| {
                    Path::new(path)
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or(path)
                        .to_string()
                });
            out.push((label, PathBuf::from(path)));
        }
        if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
            for child in children {
                walk(child, out);
            }
        }
    }
    let mut out = Vec::new();
    if let Some(devices) = value.get("blockdevices").and_then(|v| v.as_array()) {
        for dev in devices {
            walk(dev, &mut out);
        }
    }
    out
}

pub fn mount_device(device: &Path) -> Result<Option<PathBuf>> {
    let output = Command::new("udisksctl")
        .args(["mount", "-b"])
        .arg(device)
        .output()?;
    if !output.status.success() {
        return Err(anyhow!(
            "mount failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mount = text
        .split(" at ")
        .nth(1)
        .map(str::trim)
        .map(|s| s.trim_end_matches('.'))
        .filter(|s| s.starts_with('/'));
    Ok(mount.map(PathBuf::from))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

    #[test]
    fn copy_preserves_symlink_identity() {
        let td = tempfile::tempdir().unwrap();
        let src = td.path().join("src");
        let dst = td.path().join("dst");
        fs::create_dir(&src).unwrap();
        fs::create_dir(&dst).unwrap();
        fs::write(src.join("target"), b"data").unwrap();
        std::os::unix::fs::symlink("target", src.join("link")).unwrap();
        let copied = copy_item(&src, &dst).unwrap();
        assert!(
            fs::symlink_metadata(copied.join("link"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn non_utf8_copy_does_not_require_utf8_name() {
        let td = tempfile::tempdir().unwrap();
        let src_dir = td.path().join("src");
        let dst = td.path().join("dst");
        fs::create_dir(&src_dir).unwrap();
        fs::create_dir(&dst).unwrap();
        let name = OsString::from_vec(vec![b'f', b'o', 0x80]);
        let src = src_dir.join(&name);
        fs::write(&src, b"data").unwrap();
        let out = copy_item(&src, &dst).unwrap();
        assert_eq!(out.file_name(), Some(name.as_os_str()));
    }

    #[test]
    fn copy_handles_spaces_quotes_and_newlines() {
        let td = tempfile::tempdir().unwrap();
        let src_dir = td.path().join("src");
        let dst = td.path().join("dst");
        fs::create_dir(&src_dir).unwrap();
        fs::create_dir(&dst).unwrap();
        for name in [
            "space name.txt",
            "quote\"name.txt",
            "line\nname.txt",
            "snowman-☃.txt",
        ] {
            let src = src_dir.join(name);
            fs::write(&src, b"data").unwrap();
            let out = copy_item(&src, &dst).unwrap();
            assert!(out.exists());
        }
    }

    #[test]
    fn cancelled_copy_cleans_partial_destination() {
        let td = tempfile::tempdir().unwrap();
        let src_dir = td.path().join("src");
        let dst = td.path().join("dst");
        fs::create_dir(&src_dir).unwrap();
        fs::create_dir(&dst).unwrap();
        fs::write(src_dir.join("one.txt"), b"data").unwrap();
        let cancel = Arc::new(AtomicBool::new(true));
        assert!(
            copy_item_with_policy_cancel(&src_dir, &dst, CollisionPolicy::KeepBoth, Some(&cancel))
                .is_err()
        );
        assert!(!dst.join("src").exists());
    }

    #[test]
    fn copy_preserves_executable_mode() {
        let td = tempfile::tempdir().unwrap();
        let src_dir = td.path().join("src");
        let dst = td.path().join("dst");
        fs::create_dir(&src_dir).unwrap();
        fs::create_dir(&dst).unwrap();
        let src = src_dir.join("run.sh");
        fs::write(&src, b"#!/bin/sh\n").unwrap();
        let mut permissions = fs::metadata(&src).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&src, permissions).unwrap();
        let out = copy_item(&src, &dst).unwrap();
        assert_eq!(
            fs::metadata(out).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn descendant_copy_is_rejected() {
        let td = tempfile::tempdir().unwrap();
        let src = td.path().join("src");
        fs::create_dir(&src).unwrap();
        let child = src.join("child");
        fs::create_dir(&child).unwrap();
        assert!(copy_item(&src, &child).is_err());
    }
}
