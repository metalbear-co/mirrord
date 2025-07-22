use std::{io::Write, time::Duration};

use mirrord_operator::crd::policy::{BlockedFeature, MirrordPolicy, MirrordPolicySpec};
use rstest::rstest;
use tempfile::NamedTempFile;

use crate::{
    operator::policies::PolicyGuard,
    utils::{
        application::Application, kube_client, kube_service::KubeService, services::basic_service,
    },
};

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(60))]
pub async fn create_policy_and_try_scale_down_deployment(
    #[future] basic_service: KubeService,
    #[future] kube_client: kube::Client,
) {
    let kube_client = kube_client.await;
    let service = basic_service.await;

    // Create policy, delete it when test exits.
    let _policy_guard = PolicyGuard::namespaced(
        kube_client,
        &MirrordPolicy::new(
            "e2e-test-scale-down-deployment",
            MirrordPolicySpec {
                target_path: Some("*service-*".into()),
                selector: None,
                block: vec![BlockedFeature::CopyTargetScaleDown],
                env: Default::default(),
                fs: Default::default(),
                network: Default::default(),
                require_profile: Default::default(),
                profile_allowlist: Default::default(),
                applies_to_copy_targets: Default::default(),
            },
        ),
        &service.namespace,
    )
    .await;

    let application = Application::NodeFsPolicy;
    println!("Running mirrord {application:?} against {}", &service.name);

    let mirrord_config = serde_json::json!({
        "feature": {
            "copy_target": {
                "enabled": true,
                "scale_down": true
            }
        }
    });
    let mut mirrord_config_file = NamedTempFile::with_suffix(".json").unwrap();

    mirrord_config_file
        .write_all(mirrord_config.to_string().as_bytes())
        .unwrap();

    let test_proc = application
        .run(
            &service.deployment_target(),
            Some(&service.namespace),
            Some(vec![
                "--config-file",
                mirrord_config_file.path().to_str().unwrap(),
            ]),
            None,
        )
        .await;

    test_proc
        .wait_for_line(Duration::from_secs(120), "Operation was blocked by policy")
        .await;

    test_proc
        .assert_stderr_contains("Operation was blocked by policy `e2e-test-scale-down-deployment`")
        .await;
}
