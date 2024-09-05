use std::time::Duration;

use tokio::process::Command;
use tokio_retry::strategy::{jitter, ExponentialBackoff};

use crate::{config::ContainerRuntime, error::Result};

/// This works as an alternative to `--rm` flag to sidecar container (`mirrord container` command),
/// this will perform `<runtume> attach <container_id>` and once the container exits this should
/// wait 5s (to allow `mirrord container` to collect the error log) and then delete the pod.
pub async fn watcher(runtime: ContainerRuntime, container_id: &str) -> Result<()> {
    let mut retry_strategy = ExponentialBackoff::from_millis(100)
        .max_delay(Duration::from_secs(10))
        .map(jitter);

    loop {
        let mut command = Command::new(runtime.to_string());
        command.args(["attach", "--no-stdin", container_id]);

        match command.output().await {
            Ok(output) => {
                if !output.stderr.is_empty()
                    && let Ok(stderr) = String::from_utf8(output.stderr)
                {
                    tracing::error!(
                        ?container_id,
                        ?runtime,
                        ?stderr,
                        "error after running attach command"
                    );

                    break;
                }
            }
            Err(error) => {
                tracing::error!(
                    ?container_id,
                    ?error,
                    ?runtime,
                    "error running <runtime> attach <container_id>"
                );

                if let Some(backoff_timeout) = retry_strategy.next() {
                    tokio::time::sleep(backoff_timeout).await
                } else {
                    tracing::error!(
                        ?container_id,
                        ?error,
                        ?runtime,
                        "timout reached for waiting for <runtime> attach to <container_id>"
                    );

                    // TODO: maybe throw an error though the stdout is detached, the tracing is
                    // mainly for mirrord console
                    return Ok(());
                }
            }
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
