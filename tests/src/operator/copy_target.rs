#![cfg(test)]
#![cfg(feature = "operator")]
//! Test the copy-target features with an operator.

use rstest::*;

use crate::utils::{
    application::env::EnvApp, kube_service::KubeService, run_command::run_exec_with_target,
    services::basic_service,
};

/// Starts mirrord with the `copy-target` feature just to validate that it can create a
/// working copy-pod. Should work as a sanity check that the targets (see `target` parameter)
/// don't create failed copy-pods due to some incorrect pod spec.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
pub async fn copy_target_starts_a_working_copy(
    #[future]
    #[notrace]
    basic_service: KubeService,
    #[values(EnvApp::NodeInclude)] application: EnvApp,
    #[values("pod", "deployment")] target: &str,
) {
    let service = basic_service.await;

    let mut config_file = tempfile::Builder::new()
        .prefix("copy_target_starts_a_working_copy")
        .suffix(".json")
        .tempfile()
        .unwrap();
    let config = serde_json::json!({
        "feature": {
            "copy_target": {
                "scale_down": false
            }
        }
    });
    serde_json::to_writer(config_file.as_file_mut(), &config).unwrap();

    let target = match target {
        "pod" => service.pod_container_target(),
        "deployment" => service.deployment_target(),
        other => unimplemented!("Add a new branch to check for this target `{other}` `{target}`!",),
    };

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
