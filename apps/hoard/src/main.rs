mod platform;

use anyhow::Result;
use hoard_core::{cache, config, diagnostics, fs_model, identity, self_test, updater};
use std::path::PathBuf;

fn main() -> Result<()> {
    config::install_panic_hook();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("hoard {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if args.iter().any(|a| a == "--build-id") {
        println!("{}", identity::build_identity().display());
        return Ok(());
    }
    if args.iter().any(|a| a == "--diagnostics") {
        diagnostics::print_diagnostics();
        return Ok(());
    }
    if args.iter().any(|a| a == "--self-test") {
        self_test::run()?;
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
    if let Some(idx) = args.iter().position(|a| a == "--benchmark-dir") {
        let path = args
            .get(idx + 1)
            .map(PathBuf::from)
            .ok_or_else(|| anyhow::anyhow!("--benchmark-dir requires a path"))?;
        if !path.is_dir() {
            return Err(anyhow::anyhow!(
                "benchmark path is not a directory: {}",
                path.display()
            ));
        }
        let started = std::time::Instant::now();
        let entries = fs_model::read_directory(&path, true)?;
        let cold_ms = started.elapsed().as_secs_f64() * 1000.0;
        let cache_write = std::time::Instant::now();
        cache::store_directory(&path, true, &entries)?;
        let cache_write_ms = cache_write.elapsed().as_secs_f64() * 1000.0;
        let cache_read = std::time::Instant::now();
        let cached = cache::load_directory(&path, true, false).unwrap_or_default();
        let cache_read_ms = cache_read.elapsed().as_secs_f64() * 1000.0;
        println!("Hoard directory benchmark");
        println!("path: {}", path.display());
        println!("items: {}", entries.len());
        println!("cold read: {:.3} ms", cold_ms);
        println!("cache write: {:.3} ms", cache_write_ms);
        println!("cache read: {:.3} ms", cache_read_ms);
        println!("cached items: {}", cached.len());
        return Ok(());
    }
    let start_path = args
        .iter()
        .find(|a| !a.starts_with('-'))
        .map(|raw| raw.strip_prefix("file://").unwrap_or(raw))
        .map(PathBuf::from)
        .filter(|p| p.exists())
        .map(|p| {
            if p.is_file() {
                p.parent().unwrap_or(&p).to_path_buf()
            } else {
                p
            }
        });
    platform::run(start_path)
}
