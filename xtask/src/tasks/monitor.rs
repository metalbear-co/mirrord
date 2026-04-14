use std::{fs, path::Path};

use anyhow::{Context, Result};

/// Prepares the monitor frontend dist directory required by rust-embed.
pub fn build_monitor() -> Result<()> {
    println!("Preparing monitor frontend assets...");

    let monitor_dist = Path::new("monitor-frontend/dist");

    fs::create_dir_all(monitor_dist).with_context(|| {
        format!(
            "Failed to create monitor frontend dist directory at {}",
            monitor_dist.display()
        )
    })?;

    println!(
        "✓ Monitor frontend assets prepared at {}",
        monitor_dist.display()
    );
    Ok(())
}
