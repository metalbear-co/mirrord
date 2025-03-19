#![cfg(test)]

use std::time::Duration;

use rstest::*;

use crate::utils::{config_dir, service, Application, KubeService};

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

    let mut config_path = config_dir().clone();
    config_path.push("copy_target_starts_a_working_copy.json");

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
