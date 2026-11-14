#![cfg(test)]

use std::time::Duration;

use k8s_openapi::api::core::v1::Pod;
use kube::{runtime::watcher::Config, Api};
use mirrord_test_utils::run_command::run_exec_with_target;
use rstest::*;

use crate::utils::{
    application::env::EnvApp, client::kube_client, kube_service::KubeService,
    services::basic_service, watch::Watcher, AgentMode, KubeClient,
};

/// Verifies that the agent container correctly exits after all clients are gone.
///
/// This test runs only with ephemeral agents, because it's easier to get their status
/// (it's always part of the target pod status). It takes the agent mode as a value anyway, so that
/// its name says that it runs with an ephemeral agent, which `-E 'test(/Ephemeral/)'` selects.
#[cfg_attr(target_os = "windows", ignore)]
#[rstest]
#[tokio::test]
async fn agent_container_exits(
    #[future] basic_service: KubeService,
    #[future] kube_client: KubeClient,
    #[values(AgentMode::Ephemeral)] agent_mode: AgentMode,
) {
    let service = basic_service.await;
    let kube_client = kube_client.await;

    let application = EnvApp::NodeInclude;
    let mut args = application.mirrord_args().unwrap_or_default();
    args.extend(agent_mode.flags());
    let mut process = run_exec_with_target(
        application.command(),
        &service.pod_container_target(),
        Some(&service.namespace),
        Some(args),
        None,
    )
    .await;
    let res = process.wait().await;
    assert!(res.success());

    let api = Api::<Pod>::namespaced(kube_client.get_client(), &service.namespace);
    let mut watcher = Watcher::new(
        api,
        Config {
            field_selector: Some(format!("metadata.name={}", service.pod_name)),
            ..Default::default()
        },
        |pods| {
            assert_eq!(pods.len(), 1, "unexpected number of pods");
            let pod = pods.values().next().unwrap();
            let agent_status = pod
                .status
                .as_ref()
                .unwrap()
                .ephemeral_container_statuses
                .as_ref()
                .unwrap()
                .iter()
                .find(|status| status.name.starts_with("mirrord-agent"))
                .expect("status of the agent ephemeral container was not found");
            let Some(terminated) = agent_status
                .state
                .as_ref()
                .and_then(|state| state.terminated.as_ref())
            else {
                return false;
            };
            assert_eq!(terminated.exit_code, 0, "agent container failed");
            true
        },
    );

    tokio::time::timeout(Duration::from_secs(30), watcher.run())
        .await
        .expect("agent did not finish in time");
}
