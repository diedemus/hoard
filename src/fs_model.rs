use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Local};
use nix::sys::statvfs::statvfs;
use percent_encoding::{percent_decode_str, utf8_percent_encode, NON_ALPHANUMERIC};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
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
    pub accessed: Option<SystemTime>,
    pub file_type: String,
    pub metadata_loaded: bool,
}

#[derive(Clone, Debug)]
pub struct EntryMetadata {
    pub path: PathBuf,
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum SortColumn { Name, Type, Size, Modified }

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
        if !show_hidden && name.starts_with('.') { continue; }

        let is_dir = item.file_type().map(|t| t.is_dir()).unwrap_or_else(|_| p.is_dir());
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
            accessed: None,
            file_type,
            metadata_loaded: is_dir,
        });
    }
    Ok(out)
}

pub fn load_metadata(paths: &[PathBuf]) -> Vec<EntryMetadata> {
    paths.iter().filter_map(|path| {
        let meta = fs::metadata(path).ok()?;
        Some(EntryMetadata {
            path: path.clone(),
            size: if meta.is_dir() { 0 } else { meta.len() },
            modified: meta.modified().ok(),
            accessed: meta.accessed().ok(),
        })
    }).collect()
}

fn type_for_extension(ext: &str) -> String {
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
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "c" | "cc" | "cpp" | "h" | "hpp" | "go" | "java" | "kt" | "lua" | "sh" | "bash" | "zsh" | "ps1" => "Source Code".into(),
        "toml" | "json" | "yaml" | "yml" | "ini" | "conf" | "cfg" | "xml" => "Configuration".into(),
        "txt" | "md" | "log" => "Text File".into(),
        "ttf" | "otf" | "woff" | "woff2" => "Font".into(),
        "iso" | "img" => "Disk Image".into(),
        "appimage" | "exe" | "msi" | "run" => "Application".into(),
        other if other.is_empty() => "File".into(),
        other => format!("{} File", other.to_ascii_uppercase()),
    }
}

pub fn is_image_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tif" | "tiff"))
        .unwrap_or(false)
}

pub fn is_visual_media_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(
            e.to_ascii_lowercase().as_str(),
            "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tif" | "tiff" | "svg" |
            "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" | "mpeg" | "mpg" |
            "pdf" | "stl" | "obj" | "3mf" | "step" | "stp" | "ply" | "glb" | "gltf"
        ))
        .unwrap_or(false)
}

pub fn icon_badge(entry: &FileEntry) -> &'static str {
    if entry.is_dir { return ""; }
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
        if a.is_dir != b.is_dir { return b.is_dir.cmp(&a.is_dir); }
        let ord = match col {
            SortColumn::Name => a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()),
            SortColumn::Type => a.file_type.cmp(&b.file_type).then_with(|| a.name.cmp(&b.name)),
            SortColumn::Size => a.size.cmp(&b.size).then_with(|| a.name.cmp(&b.name)),
            SortColumn::Modified => a.modified.cmp(&b.modified).then_with(|| a.name.cmp(&b.name)),
        };
        if ascending { ord } else { ord.reverse() }
    });
}

pub fn format_size(bytes: u64) -> String {
    if bytes == 0 { return "—".into(); }
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut n = bytes as f64;
    let mut i = 0usize;
    while n >= 1024.0 && i < UNITS.len() - 1 { n /= 1024.0; i += 1; }
    if i == 0 { format!("{} {}", bytes, UNITS[i]) } else { format!("{:.1} {}", n, UNITS[i]) }
}

pub fn format_modified(t: Option<SystemTime>) -> String {
    t.map(|x| {
        let dt: DateTime<Local> = x.into();
        dt.format("%b %d, %Y %H:%M").to_string()
    }).unwrap_or_else(|| "—".into())
}

pub fn disk_space(path: &Path) -> Option<(u64, u64)> {
    let s = statvfs(path).ok()?;
    let block = s.block_size();
    Some((s.blocks_available() * block, s.blocks() * block))
}

pub fn discover_mounts() -> Vec<(String, PathBuf)> {
    let text = fs::read_to_string("/proc/mounts").unwrap_or_default();
    let mut out = Vec::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let dev = fields.next().unwrap_or("");
        let mount = fields.next().unwrap_or("");
        if mount == "/" || !mount.starts_with('/') { continue; }
        if !(dev.starts_with("/dev/") || mount.starts_with("/run/media/") || mount.starts_with("/mnt/")) { continue; }
        let p = PathBuf::from(mount.replace("\\040", " "));
        if !p.exists() { continue; }
        let label = p.file_name().and_then(|s| s.to_str()).unwrap_or(mount).to_string();
        if !out.iter().any(|(_, x)| x == &p) { out.push((label, p)); }
    }
    out
}

pub fn trash_root() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join(".local/share"))
        .join("Trash")
}

pub fn trash_files_dir() -> PathBuf { trash_root().join("files") }
pub fn trash_info_dir() -> PathBuf { trash_root().join("info") }

pub fn ensure_trash_dirs() {
    let _ = fs::create_dir_all(trash_files_dir());
    let _ = fs::create_dir_all(trash_info_dir());
}

pub fn trash_count() -> usize {
    fs::read_dir(trash_files_dir()).map(|rd| rd.flatten().count()).unwrap_or(0)
}

pub fn is_in_home_trash(path: &Path) -> bool {
    path.starts_with(trash_files_dir())
}

pub fn trash_item(path: &Path) -> Result<()> {
    // Keep Hoard's Trash centralized in the user's freedesktop Trash.
    // This makes items deleted from removable/external filesystems visible
    // in Hoard's Trash view and preserves restore metadata consistently.
    manual_trash(path)
}

fn manual_trash(path: &Path) -> Result<()> {
    ensure_trash_dirs();
    let original = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let name = path.file_name().and_then(|s| s.to_str()).ok_or_else(|| anyhow!("invalid item name"))?;
    let dest = unique_destination(&trash_files_dir(), name, "");

    move_item_to(path, &dest)?;

    let info_name = format!("{}.trashinfo", dest.file_name().and_then(|s| s.to_str()).unwrap_or(name));
    let mut f = fs::File::create(trash_info_dir().join(info_name))?;
    let encoded = utf8_percent_encode(&original.to_string_lossy(), NON_ALPHANUMERIC).to_string();
    let now = Local::now().format("%Y-%m-%dT%H:%M:%S");
    writeln!(f, "[Trash Info]")?;
    writeln!(f, "Path={encoded}")?;
    writeln!(f, "DeletionDate={now}")?;
    Ok(())
}

pub fn restore_trashed(path: &Path) -> Result<PathBuf> {
    if !is_in_home_trash(path) {
        return Err(anyhow!("this item is not in Hoard's home Trash"));
    }
    let file_name = path.file_name().and_then(|s| s.to_str()).ok_or_else(|| anyhow!("invalid trash item"))?;
    let info = trash_info_dir().join(format!("{file_name}.trashinfo"));
    let body = fs::read_to_string(&info).with_context(|| format!("missing trash metadata for {file_name}"))?;
    let encoded = body.lines().find_map(|l| l.strip_prefix("Path=")).ok_or_else(|| anyhow!("trash metadata has no original path"))?;
    let decoded = percent_decode_str(encoded).decode_utf8_lossy().to_string();
    let requested = PathBuf::from(decoded);
    let parent = requested.parent().ok_or_else(|| anyhow!("invalid original path"))?;
    fs::create_dir_all(parent)?;
    let dest = if requested.exists() {
        unique_destination(parent, requested.file_name().and_then(|s| s.to_str()).unwrap_or(file_name), " restored")
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
    if let Ok(status) = gio {
        if status.success() { return Ok(()); }
    }

    ensure_trash_dirs();
    remove_contents(&trash_files_dir())?;
    remove_contents(&trash_info_dir())?;
    Ok(())
}

fn remove_contents(dir: &Path) -> Result<()> {
    if !dir.exists() { return Ok(()); }
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

pub fn copy_item(src: &Path, dest_dir: &Path) -> Result<PathBuf> {
    if src.is_dir() && dest_dir.starts_with(src) {
        return Err(anyhow!("cannot copy a folder into itself"));
    }
    let name = src.file_name().and_then(|s| s.to_str()).ok_or_else(|| anyhow!("invalid source name"))?;
    let dest = unique_destination(dest_dir, name, " copy");
    copy_item_to(src, &dest)?;
    Ok(dest)
}

pub fn move_item(src: &Path, dest_dir: &Path) -> Result<PathBuf> {
    if src.parent() == Some(dest_dir) {
        return Ok(src.to_path_buf());
    }
    if src.is_dir() && dest_dir.starts_with(src) {
        return Err(anyhow!("cannot move a folder into itself"));
    }
    let name = src.file_name().and_then(|s| s.to_str()).ok_or_else(|| anyhow!("invalid source name"))?;
    let dest = unique_destination(dest_dir, name, "");
    move_item_to(src, &dest)?;
    Ok(dest)
}

pub fn duplicate_item(src: &Path) -> Result<PathBuf> {
    let parent = src.parent().ok_or_else(|| anyhow!("item has no parent"))?;
    copy_item(src, parent)
}

pub fn rename_item(src: &Path, new_name: &str) -> Result<PathBuf> {
    let trimmed = new_name.trim();
    if trimmed.is_empty() || trimmed.contains('/') { return Err(anyhow!("invalid name")); }
    let parent = src.parent().ok_or_else(|| anyhow!("item has no parent"))?;
    let dest = parent.join(trimmed);
    if dest.exists() { return Err(anyhow!("an item with that name already exists")); }
    fs::rename(src, &dest)?;
    Ok(dest)
}

pub fn create_folder(parent: &Path, name: &str) -> Result<PathBuf> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.contains('/') { return Err(anyhow!("invalid folder name")); }
    let p = parent.join(trimmed);
    if p.exists() { return Err(anyhow!("an item with that name already exists")); }
    fs::create_dir(&p)?;
    Ok(p)
}

pub fn create_text_file(parent: &Path, name: &str) -> Result<PathBuf> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.contains('/') { return Err(anyhow!("invalid file name")); }
    let p = parent.join(trimmed);
    if p.exists() { return Err(anyhow!("an item with that name already exists")); }
    fs::File::create(&p)?;
    Ok(p)
}

fn copy_item_to(src: &Path, dest: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(src)?;
    if meta.file_type().is_symlink() {
        #[cfg(unix)] {
            let target = fs::read_link(src)?;
            std::os::unix::fs::symlink(target, dest)?;
        }
    } else if meta.is_dir() {
        fs::create_dir_all(dest)?;
        for entry in WalkDir::new(src).min_depth(1).follow_links(false) {
            let entry = entry?;
            let rel = entry.path().strip_prefix(src)?;
            let target = dest.join(rel);
            if entry.file_type().is_dir() {
                fs::create_dir_all(&target)?;
            } else if entry.file_type().is_symlink() {
                #[cfg(unix)] {
                    let link = fs::read_link(entry.path())?;
                    std::os::unix::fs::symlink(link, &target)?;
                }
            } else {
                if let Some(parent) = target.parent() { fs::create_dir_all(parent)?; }
                fs::copy(entry.path(), &target)?;
            }
        }
    } else {
        if let Some(parent) = dest.parent() { fs::create_dir_all(parent)?; }
        fs::copy(src, dest)?;
    }
    Ok(())
}

fn move_item_to(src: &Path, dest: &Path) -> Result<()> {
    if let Some(parent) = dest.parent() { fs::create_dir_all(parent)?; }
    match fs::rename(src, dest) {
        Ok(()) => Ok(()),
        Err(e) if e.raw_os_error() == Some(nix::libc::EXDEV) => {
            copy_item_to(src, dest)?;
            delete_permanently(src)?;
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

fn unique_destination(dir: &Path, original_name: &str, tag: &str) -> PathBuf {
    let direct = dir.join(original_name);
    if !direct.exists() { return direct; }

    let p = Path::new(original_name);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or(original_name);
    let ext = p.extension().and_then(|s| s.to_str());
    for i in 1..10_000u32 {
        let suffix = if i == 1 { tag.to_string() } else { format!("{tag} {i}") };
        let candidate = match ext {
            Some(ext) if !ext.is_empty() => dir.join(format!("{stem}{suffix}.{ext}")),
            _ => dir.join(format!("{stem}{suffix}")),
        };
        if !candidate.exists() { return candidate; }
    }
    dir.join(format!("{original_name}.hoard-copy"))
}
