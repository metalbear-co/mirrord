#![cfg(test)]
//! Tests for rollout target regression cases to ensure continued support for Argo Rollouts.

use mirrord_test_utils::run_command::run_exec_with_target;
use rstest::*;

use crate::utils::{
    application::env::EnvApp, kube_service::KubeService, services::rollout_service,
};

/// Starts mirrord targeting a [rollout](https://argoproj.github.io/argo-rollouts/features/specification/).
///
/// The goal here is to just validate that the session is started correctly.
#[cfg_attr(target_os = "windows", ignore)]
#[rstest]
#[tokio::test]
pub async fn target_rollout(
    #[future]
    #[notrace]
    rollout_service: KubeService,
) {
    let application = EnvApp::NodeInclude;
    let service = rollout_service.await;
    let target = service.rollout_target();

    let mut process = run_exec_with_target(
        application.command(),
        &target,
        None,
        application.mirrord_args(),
        None,
    )
    .await;
    let res = process.wait().await;
    assert!(res.success());
}
