mod app;
mod config;
mod fs_model;
mod platform;
mod theme;
mod thumbnail;
mod updater;

use anyhow::Result;
use eframe::egui;
use std::path::PathBuf;

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let backend = platform::select_window_backend(&args);

    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("hoard {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    if args.iter().any(|a| a == "--update-check") {
        match updater::check_for_update()? {
            Some(info) => println!("update available: {}\n{}", info.version, info.notes),
            None => println!("hoard is up to date"),
        }
        return Ok(());
    }

    if args.iter().any(|a| a == "--update") {
        updater::install_latest_update()?;
        return Ok(());
    }

    let start_path = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .map(PathBuf::from)
        .filter(|p| p.exists());

    let mut options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Hoard")
            .with_app_id("org.diedemus.Hoard")
            .with_inner_size([1450.0, 900.0])
            .with_min_inner_size([1050.0, 650.0])
            .with_decorations(false)
            .with_transparent(true),
        ..Default::default()
    };
    let scale_restore = platform::configure_native_options(&mut options, backend);

    eframe::run_native(
        "Hoard",
        options,
        Box::new(move |cc| {
            if let Some(restore) = scale_restore { restore.restore(); }
            Ok(Box::new(app::HoardApp::new(cc, start_path)))
        }),
    )
    .map_err(|e| anyhow::anyhow!(e.to_string()))
}
