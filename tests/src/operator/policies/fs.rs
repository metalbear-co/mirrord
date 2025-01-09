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
                target_path: Some("fs_policy_e2e-test-*".into()),
                selector: None,
                block: Default::default(),
                env: Default::default(),
                fs: FsPolicy {
                    read_only: HashSet::from_iter(vec!["file.read-only".to_string()]),
                    local: HashSet::from_iter(vec!["file.local".to_string()]),
                    not_found: HashSet::from_iter(vec!["file.not-found".to_string()]),
                },
            },
        ),
        &service.namespace,
    )
    .await;

    let application = Application::NodeFsPolicy;
    println!("Running mirrord {application:?} against {}", &service.name);

    let mut test_process = application
        .run(
            &service.target,
            Some(&service.namespace),
            Some(vec!["--fs-mode=write"]),
            None,
        )
        .await;

    test_process.wait_assert_success().await;

    test_process
        .assert_stderr_contains("FAIL /app/file.local")
        .await;
    test_process
        .assert_stderr_contains("FAIL /app/file.not-found")
        .await;
    test_process
        .assert_stderr_contains("FAIL r+ /app/file.read-only")
        .await;

    test_process
        .assert_stdout_contains("SUCCESS /app/file.read-only")
        .await;
    test_process
        .assert_stdout_contains("SUCCESS /app/file.read-write")
        .await;
}
