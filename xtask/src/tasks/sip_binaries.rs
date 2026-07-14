use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

/// Version of the apple utils release to download, used to build the download URL and archive name.
///
/// Must be kept in sync with `mirrord_sip::APPLE_UTILS_VERSION`, which the CLI uses at runtime to
/// decide whether the binaries already extracted to `~/.mirrord/binaries` match this release.
const APPLE_UTILS_VERSION: &str = "v7";

/// Fetches the SIP utilities bundle once per workspace and reuses the cached archive afterwards.
pub fn download() -> Result<PathBuf> {
    let archive_path = sip_binaries_archive_path();

    if archive_path.exists() && archive_path.metadata()?.len() > 0 {
        return Ok(archive_path);
    }

    if let Some(parent) = archive_path.parent() {
        fs::create_dir_all(parent).context("Failed to create SIP archive cache directory")?;
    }

    println!("Downloading SIP utilities bundle...");
    let url = format!(
        "https://github.com/metalbear-co/appleutils/releases/download/{APPLE_UTILS_VERSION}/apple-utils-{APPLE_UTILS_VERSION}.tar.gz"
    );
    let response = reqwest::blocking::get(&url)
        .context("Failed to download SIP utilities bundle")?
        .error_for_status()
        .context("SIP utilities download returned an error status")?;
    let bytes = response
        .bytes()
        .context("Failed to read SIP utilities bundle response")?;

    fs::write(&archive_path, &bytes).context("Failed to write SIP utilities bundle archive")?;

    println!("✓ SIP utilities bundle cached: {}", archive_path.display());
    Ok(archive_path)
}

fn sip_binaries_archive_path() -> PathBuf {
    Path::new("target")
        .join("sip")
        .join(format!("apple-utils-{APPLE_UTILS_VERSION}.tar.gz"))
}
