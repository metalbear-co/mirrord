#![cfg(test)]
//! Tests for rollout target regression cases to ensure continued support for Argo Rollouts.

use rstest::*;

use crate::utils::{rollout_service, EnvApp, KubeService};

/// Starts mirrord with the `copy-target` feature just to validate that it can create a
/// working copy-pod. Should work as a sanity check that the targets (see `target` parameter)
/// don't create failed copy-pods due to some incorrect pod spec.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
pub async fn rollout_regression_copy_target(
    #[future]
    #[notrace]
    rollout_service: KubeService,
    #[values(EnvApp::NodeInclude)] application: EnvApp,
    #[values(false)] scale_down: bool,
) {
    use crate::utils::run_exec_with_target;

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
