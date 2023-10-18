use std::sync::LazyLock;

use futures::{AsyncBufReadExt, TryStreamExt};
use k8s_openapi::api::core::v1::{Pod, Toleration};
use kube::{api::LogParams, Api};
use mirrord_config::agent::{AgentConfig, LinuxCapability};
use mirrord_protocol::MeshVendor;
use regex::Regex;
use tracing::warn;

use crate::{api::container::ContainerParams, error::Result};

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
pub(super) fn get_capabilities(agent: &AgentConfig) -> Vec<LinuxCapability> {
    let disabled = agent.disabled_capabilities.clone().unwrap_or_default();

    LinuxCapability::all()
        .iter()
        .copied()
        .filter(|c| !disabled.contains(c))
        .collect()
}

pub(super) fn base_command_line(
    agent: &AgentConfig,
    params: &ContainerParams,
    mesh: Option<MeshVendor>,
) -> Vec<String> {
    let mut command_line = vec![
        "./mirrord-agent".to_owned(),
        "-l".to_owned(),
        params.port.to_string(),
    ];

    if let Some(timeout) = agent.communication_timeout {
        command_line.push("-t".to_owned());
        command_line.push(timeout.to_string());
    }

    if let Some(mesh) = mesh {
        command_line.extend(["--mesh".to_string(), mesh.to_string()]);
    }

    #[cfg(debug_assertions)]
    if agent.test_error {
        command_line.push("--test-error".to_owned());
    }

    command_line
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

    warn!("Agent did not print 'agent ready' message");
    Ok(None)
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
