use std::{ops::Not, sync::LazyLock};

use futures::{AsyncBufReadExt, TryStreamExt};
use k8s_openapi::api::core::v1::{ContainerStateWaiting, EnvVar, Pod, Toleration};
use kube::{Api, api::LogParams};
use mirrord_agent_env::envs;
use mirrord_config::agent::{AgentConfig, LinuxCapability};
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

/// Retrieve a list of Linux capabilities for the agent container.
pub(super) fn get_capabilities(agent: &AgentConfig) -> Vec<LinuxCapability> {
    LinuxCapability::all()
        .iter()
        .copied()
        .filter(|c| {
            agent
                .disabled_capabilities
                .as_deref()
                .unwrap_or_default()
                .iter()
                .any(|disabled| *disabled == c.as_spec_str())
                .not()
        })
        .collect()
}

/// Builds mirrord agent environment variables.
pub(super) fn agent_env(agent: &AgentConfig, params: &ContainerParams) -> Vec<EnvVar> {
    let mut env = vec![
        envs::LOG_LEVEL.as_k8s_spec(&agent.log_level),
        envs::STEALER_FLUSH_CONNECTIONS.as_k8s_spec(&agent.flush_connections),
        envs::JSON_LOG.as_k8s_spec(&agent.json_log),
        envs::IPV6_SUPPORT.as_k8s_spec(&params.support_ipv6),
        // TODO remove after some time.
        // Left for compatibility with older agents.
        envs::PASSTHROUGH_MIRRORING.as_k8s_spec(&true),
        envs::MAX_BODY_BUFFER_SIZE.as_k8s_spec(&agent.max_body_buffer_size),
        envs::MAX_BODY_BUFFER_TIMEOUT.as_k8s_spec(&agent.max_body_buffer_timeout),
        envs::JAQ_TIME_LIMIT.as_k8s_spec(&agent.jaq_time_limit),
    ];

    if let Some(nftables) = agent.nftables {
        env.push(envs::NFTABLES.as_k8s_spec(&nftables));
    }

    if let Some(attempts) = agent.dns.attempts {
        env.push(envs::DNS_ATTEMPTS.as_k8s_spec(&attempts));
    }

    if let Some(timeout) = agent.dns.timeout {
        env.push(envs::DNS_TIMEOUT.as_k8s_spec(&timeout));
    };

    if let Some(pod_ips) = &params.pod_ips {
        env.push(envs::POD_IPS.as_k8s_spec(pod_ips));
    }

    if let Some(metrics_address) = agent.metrics.as_ref() {
        env.push(envs::METRICS.as_k8s_spec(metrics_address));
    }

    if let Some(cert) = &params.tls_cert {
        env.push(envs::OPERATOR_CERT.as_k8s_spec(cert));
    }

    if params.steal_tls_config.is_empty().not() {
        env.push(envs::STEAL_TLS_CONFIG.as_k8s_spec(&params.steal_tls_config));
    }

    if params.idle_ttl.is_zero().not() {
        env.push(envs::IDDLE_TTL.as_k8s_spec(&params.idle_ttl.as_secs()))
    }

    if agent.inject_headers {
        env.push(envs::INJECT_HEADERS.as_k8s_spec(&agent.inject_headers));
    }

    if let Some(clean) = agent.clean_iptables_on_start {
        env.push(envs::CLEAN_IPTABLES_ON_START.as_k8s_spec(&clean));
    }

    env
}

/// Static [`Container::command`](k8s_openapi::api::core::v1::Container::command) to use with agent
/// containers.
///
/// Keeping the command static enables matching the agent pods with GKE Autopilot [WorkloadAllowlist](https://docs.cloud.google.com/kubernetes-engine/docs/reference/crds/workloadallowlist).
/// Any dynamic configuration should be passed in
/// [`Container::args`](k8s_openapi::api::core::v1::Container::args) or
/// [`Container::env`](k8s_openapi::api::core::v1::Container::env).
pub(super) const AGENT_COMMAND: &str = "./mirrord-agent";

pub(super) fn agent_base_args(agent: &AgentConfig, params: &ContainerParams) -> Vec<String> {
    let mut args = vec!["-l".to_owned(), params.port.to_string()];
    if let Some(timeout) = agent.communication_timeout {
        args.push("-t".to_owned());
        args.push(timeout.to_string());
    }

    #[cfg(debug_assertions)]
    if agent.test_error {
        args.push("--test-error".to_owned());
    }

    args
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
    let log_params = LogParams {
        follow: true,
        container: Some(container_name),
        ..LogParams::default()
    };

    let logs = pod_api.log_stream(pod_name, &log_params).await?;

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

/// Returns an error message if the container is in an image pull failure state
/// (e.g. ImagePullBackOff, ErrImagePull).
pub(super) fn is_image_pull_error(container_state: &ContainerStateWaiting) -> Option<String> {
    let reason = container_state.reason.as_deref();
    if matches!(reason, Some("ImagePullBackOff" | "ErrImagePull")) {
        return Some(format!(
            "agent container failed to pull image ({}): {}",
            reason.unwrap_or("<unknown reason>"),
            container_state.message.as_deref().unwrap_or("<no message>"),
        ));
    }
    None
}

/// Inspects the ephemeral container status and returns an error message if the
/// container is stuck in an image pull failure state.
///
/// These states do not transition the pod to `Failed`, so they must be handled
/// explicitly to avoid waiting indefinitely.
pub(super) fn get_ephemeral_container_image_pull_error(
    pod: &Pod,
    container_name: &str,
) -> Option<String> {
    let waiting = pod
        .status
        .as_ref()
        .and_then(|status| status.ephemeral_container_statuses.as_deref())
        .and_then(|container_statuses| {
            container_statuses
                .iter()
                .find(|status| status.name == container_name)
                .and_then(|status| status.state.as_ref())
                .and_then(|state| state.waiting.as_ref())
        })?;

    is_image_pull_error(waiting)
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
