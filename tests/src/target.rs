#![cfg(test)]

use std::time::Duration;

use rstest::*;

use crate::utils::{
    application::env::EnvApp, kube_service::KubeService, run_command::run_exec_with_target,
    services::basic_service,
};

/// Verifies that the pod target argument can be a glob pattern.
#[cfg_attr(target_os = "windows", ignore)]
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(240))]
#[cfg_attr(not(feature = "ephemeral"), ignore)]
async fn glob_pattern_in_pod_target(#[future] basic_service: KubeService) {
    let service = basic_service.await;

    let application = EnvApp::Bash;
    let mut process = run_exec_with_target(
        application.command(),
        &format!("pod/{}*", &service.pod_name[..service.pod_name.len() - 4]),
        Some(&service.namespace),
        application.mirrord_args(),
        Some(vec![("MIRRORD_EPHEMERAL_CONTAINER", "true")]),
    )
    .await;
    let res = process.wait().await;
    assert!(res.success());
}

/// Verifies that the deployment target argument can be a glob pattern.
#[cfg_attr(target_os = "windows", ignore)]
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(240))]
#[cfg_attr(not(feature = "ephemeral"), ignore)]
async fn glob_pattern_in_deployment_target(#[future] basic_service: KubeService) {
    let service = basic_service.await;

    let application = EnvApp::Bash;
    let mut process = run_exec_with_target(
        application.command(),
        &format!("deployment/{}*", &service.name[..service.name.len() - 4]),
        Some(&service.namespace),
        application.mirrord_args(),
        Some(vec![("MIRRORD_EPHEMERAL_CONTAINER", "true")]),
    )
    .await;
    let res = process.wait().await;
    assert!(res.success());
}
