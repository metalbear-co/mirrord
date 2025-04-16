#![cfg(test)]
#![cfg(feature = "job")]
//! Tests for rollout target regression cases to ensure continued support for Argo Rollouts.

use core::time::Duration;

use rstest::*;

use crate::utils::{rollout_service, run_exec_with_target, EnvApp, KubeService};

/// Starts mirrord targeting a [rollout](https://argoproj.github.io/argo-rollouts/features/specification/).
///
/// The goal here is to just validate that the session is started correctly.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(240))]
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

/// Starts mirrord with the `copy-target` feature targeting a
/// [rollout](https://argoproj.github.io/argo-rollouts/features/specification/).
///
/// The goal here is to just validate that the session is started correctly.
#[cfg(feature = "operator")]
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(240))]
pub async fn rollout_regression_copy_target(
    #[future]
    #[notrace]
    rollout_service: KubeService,
    #[values(false, true)] scale_down: bool,
) {
    let application = EnvApp::NodeInclude;
    let service = rollout_service.await;

    let mut config_file = tempfile::Builder::new()
        .prefix("copy_target_starts_a_working_copy")
        .suffix(".json")
        .tempfile()
        .unwrap();
    let config = serde_json::json!({
        "feature": {
            "copy_target": {
                "scale_down": scale_down
            }
        }
    });
    serde_json::to_writer(config_file.as_file_mut(), &config).unwrap();

    let target = service.rollout_target();

    let mirrord_args = {
        let mut args = application.mirrord_args().unwrap_or_default();
        args.push("--config-file");
        args.push(config_file.path().to_str().unwrap());
        Some(args)
    };

    let mut process =
        run_exec_with_target(application.command(), &target, None, mirrord_args, None).await;
    let res = process.wait().await;
    assert!(res.success());
}
