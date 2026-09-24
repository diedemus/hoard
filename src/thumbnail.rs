use egui::ColorImage;
use image::{DynamicImage, ImageReader};
use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::{
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
        Arc, Mutex,
    },
    thread,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ThumbnailKey {
    pub path: PathBuf,
    pub max_side: u32,
}

#[derive(Debug)]
pub struct ThumbnailResult {
    pub key: ThumbnailKey,
    pub image: Option<ColorImage>,
}

pub struct ThumbnailLoader {
    tx: SyncSender<ThumbnailKey>,
    rx: Receiver<ThumbnailResult>,
}

impl ThumbnailLoader {
    pub fn new(workers: usize, repaint_ctx: egui::Context) -> Self {
        // Keep the queue bounded. On huge media folders this prevents scrolling
        // from creating hundreds of stale decode/render jobs.
        let (job_tx, job_rx) = mpsc::sync_channel::<ThumbnailKey>(48);
        let (result_tx, result_rx) = mpsc::channel::<ThumbnailResult>();
        let shared_rx = Arc::new(Mutex::new(job_rx));

        for _ in 0..workers.max(1) {
            let jobs = Arc::clone(&shared_rx);
            let results = result_tx.clone();
            let repaint = repaint_ctx.clone();
            thread::spawn(move || loop {
                let job = {
                    let Ok(rx) = jobs.lock() else { break; };
                    match rx.recv() {
                        Ok(job) => job,
                        Err(_) => break,
                    }
                };
                let image = decode_thumbnail(&job.path, job.max_side);
                if results.send(ThumbnailResult { key: job, image }).is_err() {
                    break;
                }
                repaint.request_repaint();
            });
        }

        Self { tx: job_tx, rx: result_rx }
    }

    pub fn request(&self, key: ThumbnailKey) -> bool {
        match self.tx.try_send(key) {
            Ok(()) => true,
            Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => false,
        }
    }

    pub fn try_recv(&self) -> Option<ThumbnailResult> {
        match self.rx.try_recv() {
            Ok(result) => Some(result),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }
}

pub fn decode_thumbnail(path: &Path, max_side: u32) -> Option<ColorImage> {
    let max_side = max_side.clamp(32, 512);
    let cache = cache_path(path, max_side);

    if cache.is_file() {
        if let Some(image) = read_cached(&cache) {
            return Some(image);
        }
        let _ = fs::remove_file(&cache);
    }

    if let Some(parent) = cache.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();

    if matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "webp" | "gif" | "bmp" | "tif" | "tiff") {
        let reader = ImageReader::open(path).ok()?.with_guessed_format().ok()?;
        let decoded = reader.decode().ok()?;
        let thumb = decoded.thumbnail(max_side, max_side);
        let _ = thumb.save_with_format(&cache, image::ImageFormat::Png);
        return color_image_from_dynamic(thumb);
    }

    let generated = match ext.as_str() {
        "svg" => thumbnail_svg(path, &cache, max_side),
        "mp4" | "mkv" | "webm" | "mov" | "avi" | "m4v" | "mpeg" | "mpg" => {
            thumbnail_video(path, &cache, max_side)
        }
        "pdf" => thumbnail_pdf(path, &cache, max_side),
        "stl" | "obj" | "3mf" | "step" | "stp" | "ply" | "glb" | "gltf" => {
            thumbnail_model(path, &cache, max_side)
        }
        _ => false,
    };

    if generated { read_cached(&cache) } else { None }
}

fn read_cached(path: &Path) -> Option<ColorImage> {
    let decoded = ImageReader::open(path).ok()?.with_guessed_format().ok()?.decode().ok()?;
    color_image_from_dynamic(decoded)
}

fn command_ok(command: &mut Command) -> bool {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn thumbnail_video(path: &Path, cache: &Path, max_side: u32) -> bool {
    command_ok(
        Command::new("ffmpegthumbnailer")
            .arg("-i").arg(path)
            .arg("-o").arg(cache)
            .arg("-s").arg(max_side.to_string())
            .arg("-t").arg("10%")
            .arg("-q").arg("8")
    ) && cache.is_file()
}

fn thumbnail_pdf(path: &Path, cache: &Path, max_side: u32) -> bool {
    let prefix = cache.with_extension("");
    let ok = command_ok(
        Command::new("pdftoppm")
            .arg("-f").arg("1")
            .arg("-singlefile")
            .arg("-scale-to").arg(max_side.to_string())
            .arg("-png")
            .arg(path)
            .arg(&prefix)
    );
    ok && cache.is_file()
}

fn thumbnail_svg(path: &Path, cache: &Path, max_side: u32) -> bool {
    command_ok(
        Command::new("rsvg-convert")
            .arg("--keep-aspect-ratio")
            .arg("--width").arg(max_side.to_string())
            .arg("--height").arg(max_side.to_string())
            .arg("--output").arg(cache)
            .arg(path)
    ) && cache.is_file()
}

fn thumbnail_model(path: &Path, cache: &Path, max_side: u32) -> bool {
    // OpenSCAD gives STL/3MF/OBJ-style assets a useful visual preview when it
    // is available. Unsupported formats simply fall back to Hoard's typed icon.
    let size = format!("{},{}", max_side, max_side);
    command_ok(
        Command::new("openscad")
            .arg("--autocenter")
            .arg("--viewall")
            .arg("--imgsize").arg(size)
            .arg("-o").arg(cache)
            .arg(path)
    ) && cache.is_file()
}

fn cache_path(path: &Path, max_side: u32) -> PathBuf {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    max_side.hash(&mut hasher);
    if let Ok(meta) = fs::metadata(path) {
        meta.len().hash(&mut hasher);
        if let Ok(modified) = meta.modified() {
            if let Ok(d) = modified.duration_since(std::time::UNIX_EPOCH) {
                d.as_nanos().hash(&mut hasher);
            }
        }
    }
    let root = dirs::cache_dir().unwrap_or_else(|| PathBuf::from(".cache")).join("hoard/thumbnails");
    root.join(format!("{:016x}.png", hasher.finish()))
}

pub fn decode_display_image(path: &Path, rotation_quarters: u8, max_side: u32) -> Option<ColorImage> {
    let reader = ImageReader::open(path).ok()?.with_guessed_format().ok()?;
    let mut decoded = reader.decode().ok()?;

    for _ in 0..(rotation_quarters % 4) {
        decoded = decoded.rotate90();
    }

    let (w, h) = (decoded.width(), decoded.height());
    let max_dim = w.max(h);
    if max_dim > max_side {
        decoded = decoded.thumbnail(max_side, max_side);
    }

    color_image_from_dynamic(decoded)
}

fn color_image_from_dynamic(decoded: DynamicImage) -> Option<ColorImage> {
    let rgba = decoded.to_rgba8();
    let (w, h) = rgba.dimensions();
    if w == 0 || h == 0 {
        return None;
    }
    Some(ColorImage::from_rgba_unmultiplied(
        [w as usize, h as usize],
        rgba.as_raw(),
    ))
}
