use crate::config;
use anyhow::{Context, Result, anyhow};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateManifest {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    pub artifacts: HashMap<String, UpdateArtifact>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpdateArtifact {
    pub url: String,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct UpdateInfo {
    pub version: String,
    pub notes: String,
    artifact_url: String,
    sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Clone, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

fn target_key() -> String {
    format!("{}-unknown-linux-gnu", std::env::consts::ARCH)
}

fn client() -> Result<reqwest::blocking::Client> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(format!(
            "hoard/{} (+https://github.com)",
            env!("CARGO_PKG_VERSION")
        ))
        .build()?)
}

fn configured_repository() -> Option<String> {
    let cfg = config::load_update_config();
    let configured = cfg.repository.trim();
    if !configured.is_empty() {
        return Some(configured.to_string());
    }
    if let Some(repo) = option_env!("HOARD_GITHUB_REPOSITORY")
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Some(repo.to_string());
    }
    Some(config::DEFAULT_GITHUB_REPOSITORY.to_string())
}

fn github_update_info() -> Result<UpdateInfo> {
    let cfg = config::load_update_config();
    if !cfg.enabled {
        return Err(anyhow!(
            "updates are disabled; edit {}",
            config::update_config_path().display()
        ));
    }

    let repo = configured_repository().ok_or_else(|| {
        anyhow!(
            "GitHub update repository is not configured; set repository = \"OWNER/hoard\" in {}",
            config::update_config_path().display()
        )
    })?;

    let api = format!("https://api.github.com/repos/{repo}/releases/latest");
    let release: GitHubRelease = client()?
        .get(api)
        .header("Accept", "application/vnd.github+json")
        .send()?
        .error_for_status()?
        .json()
        .context("invalid GitHub release response")?;

    if release.draft || release.prerelease {
        return Err(anyhow!("GitHub latest release is not a stable release"));
    }

    let remote_text = release.tag_name.trim_start_matches('v');
    let remote = Version::parse(remote_text)
        .with_context(|| format!("GitHub release tag {:?} is not semver", release.tag_name))?;

    let target = target_key();
    let bundle_name = format!("hoard-{remote}-{target}.tar.zst");
    let checksum_name = format!("{bundle_name}.sha256");

    let bundle = release
        .assets
        .iter()
        .find(|a| a.name == bundle_name)
        .ok_or_else(|| {
            anyhow!(
                "GitHub release {} has no asset named {}",
                release.tag_name,
                bundle_name
            )
        })?;
    let checksum = release
        .assets
        .iter()
        .find(|a| a.name == checksum_name)
        .ok_or_else(|| {
            anyhow!(
                "GitHub release {} has no checksum asset named {}",
                release.tag_name,
                checksum_name
            )
        })?;

    let checksum_text = client()?
        .get(&checksum.browser_download_url)
        .send()?
        .error_for_status()?
        .text()?;
    let sha256 = checksum_text
        .split_whitespace()
        .next()
        .filter(|s| s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or_else(|| anyhow!("invalid SHA-256 file published with GitHub release"))?
        .to_ascii_lowercase();

    Ok(UpdateInfo {
        version: remote.to_string(),
        notes: release.body.unwrap_or_default(),
        artifact_url: bundle.browser_download_url.clone(),
        sha256,
    })
}

fn legacy_manifest_update_info() -> Result<UpdateInfo> {
    let cfg = config::load_update_config();
    if !cfg.enabled || cfg.manifest_url.trim().is_empty() {
        return Err(anyhow!(
            "updates are not configured; edit {}",
            config::update_config_path().display()
        ));
    }
    let manifest: UpdateManifest = client()?
        .get(&cfg.manifest_url)
        .send()?
        .error_for_status()?
        .json()
        .context("invalid legacy update manifest")?;
    let key = target_key();
    let artifact = manifest
        .artifacts
        .get(&key)
        .ok_or_else(|| anyhow!("release {} has no artifact for {}", manifest.version, key))?;
    Ok(UpdateInfo {
        version: manifest.version.clone(),
        notes: manifest.notes.clone(),
        artifact_url: artifact.url.clone(),
        sha256: artifact.sha256.clone(),
    })
}

fn latest_update_info() -> Result<UpdateInfo> {
    let cfg = config::load_update_config();
    if configured_repository().is_some() {
        github_update_info()
    } else if !cfg.manifest_url.trim().is_empty() {
        // v0.3 compatibility: old configs had only manifest_url.
        legacy_manifest_update_info()
    } else if cfg.provider.eq_ignore_ascii_case("github") {
        github_update_info()
    } else {
        legacy_manifest_update_info()
    }
}

pub fn check_for_update() -> Result<Option<UpdateInfo>> {
    let info = latest_update_info()?;
    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let remote = Version::parse(&info.version)?;
    if remote > current {
        Ok(Some(info))
    } else {
        Ok(None)
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

pub fn install_latest_update() -> Result<()> {
    let Some(info) = check_for_update()? else {
        println!("Hoard {} is already current.", env!("CARGO_PKG_VERSION"));
        return Ok(());
    };

    println!("Downloading Hoard {} from GitHub Releases…", info.version);
    let bytes = client()?
        .get(&info.artifact_url)
        .send()?
        .error_for_status()?
        .bytes()?;

    let digest = sha256_hex(&bytes);
    if !digest.eq_ignore_ascii_case(&info.sha256) {
        return Err(anyhow!("SHA-256 mismatch; refusing update"));
    }

    let tmp = tempfile::tempdir()?;
    let decoder = zstd::stream::read::Decoder::new(Cursor::new(bytes))?;
    let mut archive = tar::Archive::new(decoder);
    archive.unpack(tmp.path())?;

    let payload = tmp.path().join("hoard-release");
    let bin_src = payload.join("bin/hoard");
    if !bin_src.is_file() {
        return Err(anyhow!("release payload missing binary"));
    }

    let home = dirs::home_dir().ok_or_else(|| anyhow!("cannot determine HOME"))?;
    let bin_dir = home.join(".local/bin");
    fs::create_dir_all(&bin_dir)?;
    let bin_dst = bin_dir.join("hoard");
    let staged = bin_dir.join(".hoard.update");
    let previous = bin_dir.join(".hoard.previous");
    fs::copy(&bin_src, &staged)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut p = fs::metadata(&staged)?.permissions();
        p.set_mode(0o755);
        fs::set_permissions(&staged, p)?;
    }

    let staged_version = Command::new(&staged)
        .arg("--version")
        .output()
        .context("could not execute staged Hoard update")?;
    let expected = format!("hoard {}", info.version);
    let reported = String::from_utf8_lossy(&staged_version.stdout)
        .trim()
        .to_string();
    if !staged_version.status.success() || reported != expected {
        let _ = fs::remove_file(&staged);
        return Err(anyhow!(
            "staged update verification failed: expected {expected:?}, got {reported:?}"
        ));
    }

    let _ = fs::remove_file(&previous);
    if bin_dst.exists() {
        fs::rename(&bin_dst, &previous)?;
    }
    if let Err(error) = fs::rename(&staged, &bin_dst) {
        if previous.exists() {
            let _ = fs::rename(&previous, &bin_dst);
        }
        return Err(error.into());
    }

    let installed = Command::new(&bin_dst).arg("--version").output();
    let installed_ok = installed
        .as_ref()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == expected)
        .unwrap_or(false);
    if !installed_ok {
        let _ = fs::remove_file(&bin_dst);
        if previous.exists() {
            fs::rename(&previous, &bin_dst)?;
        }
        return Err(anyhow!(
            "installed update failed verification; previous Hoard was restored"
        ));
    }

    let share = payload.join("share");
    if share.exists() {
        copy_tree(&share, &home.join(".local/share"))?;
    }
    let _ = fs::remove_file(&previous);

    println!("Updated to Hoard {} from GitHub Releases.", info.version);
    Ok(())
}

fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst)?;
    for item in fs::read_dir(src)? {
        let item = item?;
        let from = item.path();
        let to = dst.join(item.file_name());
        if item.file_type()?.is_dir() {
            copy_tree(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

#[allow(dead_code)]
pub fn update_config_path() -> PathBuf {
    config::update_config_path()
}
