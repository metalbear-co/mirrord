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

    // rustls can't pick a crypto provider when it's built with more than one, as it is under Bazel
    // (see `MODULE.bazel`), so this installs `aws_lc_rs` as the default, as the CLI's `main` does.
    rustls::crypto::CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider())
        .expect("Failed to install crypto provider");

    // No telemetry: this binary exists for working on the interface itself.
    mirrord_tui::run(None, None).await
}
