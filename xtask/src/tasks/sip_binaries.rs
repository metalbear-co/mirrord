use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};

const APPLE_UTILS_URL: &str =
    "https://github.com/metalbear-co/appleutils/releases/download/v5/apple-utils-v5.tar.gz";
const APPLE_UTILS_ARCHIVE_NAME: &str = "apple-utils-v5.tar.gz";

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
    let response = reqwest::blocking::get(APPLE_UTILS_URL)
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
        .join(APPLE_UTILS_ARCHIVE_NAME)
}
