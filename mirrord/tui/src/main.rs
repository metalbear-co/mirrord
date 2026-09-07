//! The standalone `mirrord-tui` binary.
//!
//! Everything the interface itself does lives in the library, which is also what the CLI's
//! `mirrord tui` subcommand calls. What is left here is what a process that exists *only* to show
//! the interface is entitled to do: own the global logger.

use std::{fs::File, sync::Arc};

use anyhow::Context;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if let Ok(path) = std::env::var("MIRRORD_LOG_FILE") {
        let file = File::create(path).context("failed to create log file")?;

        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::from_env("MIRRORD_LOG"))
            .with_writer(Arc::new(file))
            .init();
    }

    // No telemetry: this binary exists for working on the interface itself.
    mirrord_tui::run(None).await
}
