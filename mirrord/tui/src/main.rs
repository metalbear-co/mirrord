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

    mirrord_tui::run().await
}
