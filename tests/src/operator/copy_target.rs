#![cfg(test)]
// #![cfg(feature = "operator")]
//! Test the copy-target features with an operator.

use std::time::Duration;

use rstest::*;
use tempfile::NamedTempFile;

use crate::utils::{service, Application, KubeService};

/// Starts mirrord with the `copy-target` feature just to validate that it can create a
/// working copy-pod. Should work as a sanity check that the targets (see `target` parameter)
/// don't create failed copy-pods due to some incorrect pod spec.
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
pub async fn copy_target_starts_a_working_copy(
    #[future]
    #[notrace]
    service: KubeService,
    #[values(Application::CopyTargetStartsAWorkingCopy)] application: Application,
    #[values("pod", "deployment")] target: &str,
) {
    let service = service.await;

    // Create a temporary file for the config
    let mut config_file = NamedTempFile::with_suffix(".json").unwrap();

    // Create the config JSON
    let config = serde_json::json!({
        "feature": {
            "copy_target": {
                "scale_down": false
            }
        }
    });
    // Write the config to the temporary file
    serde_json::to_writer(config_file.as_file_mut(), &config).unwrap();

    let target = match target {
        "pod" => service.pod_container_target(),
        "deployment" => service.deployment_target(),
        other => unimplemented!("Add a new branch to check for this target `{other}` `{target}`!",),
    };

    let mut test_proc = application
        .run(
            &target,
            Some(&service.namespace),
            Some(vec!["--config-file", config_path.to_str().unwrap()]),
            None,
        )
        .await;

    test_proc
        .wait_for_line(Duration::from_secs(40), "daemon subscribed")
        .await;

    test_proc.assert_no_error_in_stdout().await;
    test_proc.wait_assert_success().await;
}
