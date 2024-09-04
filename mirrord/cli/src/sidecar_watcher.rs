use std::time::Duration;

use tokio::process::Command;

use crate::{config::ContainerRuntime, error::Result};

pub async fn watcher(runtime: ContainerRuntime, container_id: &str) -> Result<()> {
    let mut command = Command::new(runtime.to_string());
    command.args(["attach", "--no-stdin", container_id]);

    match command.output().await {
        Ok(output) => {
            if !output.stderr.is_empty()
                && let Ok(stderr) = String::from_utf8(output.stderr)
            {
                tracing::error!(?stderr, "error after running attach command");
            }
        }
        Err(error) => {
            tracing::error!(?error, "error running <runtime> attach <container_id>");
        }
    }

    tracing::info!("exit detected waiting 5s before delete");
    tokio::time::sleep(Duration::from_secs(5)).await;

    let mut command = Command::new(runtime.to_string());
    command.args(["rm", container_id]);

    match command.output().await {
        Ok(output) => {
            if !output.stderr.is_empty()
                && let Ok(stderr) = String::from_utf8(output.stderr)
            {
                tracing::error!(?stderr, "error after running rm command");
            }
        }
        Err(error) => {
            tracing::error!(?error, "error running <runtime> rm <container_id>");
        }
    }

    Ok(())
}
