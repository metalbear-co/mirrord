use std::{collections::HashSet, time::Duration};

use mirrord_operator::crd::policy::{FsPolicy, MirrordPolicy, MirrordPolicySpec};
use rstest::{fixture, rstest};

use crate::{
    operator::policies::PolicyGuard,
    utils::{kube_client, service, Application, KubeService},
};

#[fixture]
async fn fs_service(#[future] kube_client: kube::Client) -> KubeService {
    let namespace = format!("e2e-tests-fs-policies-{}", crate::utils::random_string());

    service(
        &namespace,
        "NodePort",
        "ghcr.io/metalbear-co/mirrord-pytest:latest",
        "fs-policy-e2e-test-service",
        false,
        kube_client,
    )
    .await
}

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(60))]
pub async fn create_namespaced_fs_policy_and_try_file_open(
    #[future] fs_service: KubeService,
    #[future] kube_client: kube::Client,
) {
    let kube_client = kube_client.await;
    let service = fs_service.await;

    // Create policy, delete it when test exits.
    let _policy_guard = PolicyGuard::namespaced(
        kube_client,
        &MirrordPolicy::new(
            "e2e-test-fs-policy-with-path-pattern",
            MirrordPolicySpec {
                target_path: Some("*fs-policy-e2e-test-*".into()),
                selector: None,
                block: Default::default(),
                env: Default::default(),
                fs: FsPolicy {
                    read_only: HashSet::from_iter(vec!["file\\.read-only".to_string()]),
                    local: HashSet::from_iter(vec!["file\\.local".to_string()]),
                    not_found: HashSet::from_iter(vec!["file\\.not-found".to_string()]),
                },
                network: Default::default(),
                require_profile: Default::default(),
                profile_allowlist: Default::default(),
                applies_to_copy_targets: false,
            },
        ),
        &service.namespace,
    )
    .await;

    let application = Application::NodeFsPolicy;
    println!("Running mirrord {application:?} against {}", &service.name);

    let mut test_process = application
        .run(
            &service.pod_container_target(),
            Some(&service.namespace),
            Some(vec!["--fs-mode=write"]),
            None,
        )
        .await;

    test_process.wait_assert_success().await;

    let stdout = test_process.get_stdout().await;

    let reading_local_failed = stdout.contains("FAIL r /app/file.local");
    let reading_not_found_failed = stdout.contains("FAIL r /app/file.not-found");
    let reading_read_only_succeeded = stdout.contains("SUCCESS r /app/file.read-only");
    let writing_read_only_failed = stdout.contains("FAIL r+ /app/file.read-only");
    let writing_read_write_succeeded = stdout.contains("SUCCESS r+ /app/file.read-write");

    assert!(
        reading_local_failed
            && reading_not_found_failed
            && reading_read_only_succeeded
            && writing_read_only_failed
            && writing_read_write_succeeded,
        "some file operations did not finish as expected:\n
        \treading_local_failed={reading_local_failed}\n
        \treading_not_found_failed={reading_not_found_failed}\n
        \treading_read_only_succeeded={reading_read_only_succeeded} \n
        \twriting_read_only_failed={writing_read_only_failed}\n
        \twriting_read_write_succeeded={writing_read_write_succeeded}",
    )
}
