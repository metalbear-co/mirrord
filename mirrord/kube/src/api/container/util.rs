use std::sync::LazyLock;

use futures::{AsyncBufReadExt, TryStreamExt};
use k8s_openapi::api::core::v1::{Pod, Toleration};
use kube::{api::LogParams, Api};
use mirrord_config::agent::{AgentConfig, LinuxCapability};
use regex::Regex;

use crate::error::{KubeApiError, Result};

static AGENT_READY_REGEX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new("agent ready( - version (\\S+))?").expect("failed to create regex")
});

pub(super) static DEFAULT_TOLERATIONS: LazyLock<Vec<Toleration>> = LazyLock::new(|| {
    vec![Toleration {
        operator: Some("Exists".to_owned()),
        ..Default::default()
    }]
});

pub(super) fn get_agent_image(agent: &AgentConfig) -> String {
    match &agent.image {
        Some(image) => image.clone(),
        None => concat!("ghcr.io/metalbear-co/mirrord:", env!("CARGO_PKG_VERSION")).to_owned(),
    }
}

/// Retrieve a list of Linux capabilities for the agent container.
pub(super) fn get_capabilities(config: &AgentConfig) -> Vec<LinuxCapability> {
    let disabled = config.disabled_capabilities.clone().unwrap_or_default();

    LinuxCapability::all()
        .iter()
        .copied()
        .filter(|c| !disabled.contains(c))
        .collect()
}

/**
 * Wait until the agent prints the "agent ready" message.
 * Return agent version extracted from the message (if found).
 */
#[tracing::instrument(level = "trace", skip(pod_api), ret)]
pub(super) async fn wait_for_agent_startup(
    pod_api: &Api<Pod>,
    pod_name: &str,
    container_name: String,
) -> Result<Option<String>> {
    let logs = pod_api
        .log_stream(
            pod_name,
            &LogParams {
                follow: true,
                container: Some(container_name),
                ..LogParams::default()
            },
        )
        .await?;

    let mut lines = logs.lines();
    while let Some(line) = lines.try_next().await? {
        let Some(captures) = AGENT_READY_REGEX.captures(&line) else {
            continue;
        };

        let version = captures.get(2).map(|m| m.as_str().to_string());
        return Ok(version);
    }

    Err(KubeApiError::AgentReadyMessageMissing)
}

#[cfg(test)]
mod test {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("agent ready", None)]
    #[case("agent ready - version 3.56.0", Some("3.56.0"))]
    fn agent_version_regex(#[case] agent_message: &str, #[case] version: Option<&str>) {
        let captures = AGENT_READY_REGEX.captures(agent_message).unwrap();

        assert_eq!(captures.get(2).map(|c| c.as_str()), version);
    }
}
