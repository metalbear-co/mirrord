#![cfg(test)]
#![cfg(feature = "operator")]
//! Test that mirrordpolicies work and features are blocked.

use std::{
    fmt::{Display, Formatter},
    time::Duration,
};

use kube::Api;
use mirrord_operator::crd::{BlockedFeature, MirrordPolicy, MirrordPolicySpec};
use rstest::{fixture, rstest};

use crate::utils::{kube_client, service, Application, KubeService, ResourceGuard, TestProcess};

/// Guard that deletes a mirrord policy when dropped.
struct PolicyGuard(ResourceGuard);

impl PolicyGuard {
    pub async fn new(kube_client: kube::Client, policy: &MirrordPolicy, namespace: &str) -> Self {
        let policy_api: Api<MirrordPolicy> = Api::namespaced(kube_client.clone(), namespace);
        PolicyGuard(
            ResourceGuard::create(
                policy_api,
                policy.metadata.name.clone().unwrap(),
                policy,
                true,
            )
            .await
            .expect("Could not create policy in E2E test."),
        )
    }
}

enum Target {
    Pod(String),
    Deployment(String),
}

impl Display for Target {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Target::Pod(name) => write!(f, "{}/{}", "pod", name),
            Target::Deployment(name) => write!(f, "{}/{}", "deployment", name),
        }
    }
}

struct PolicyTestCase {
    policy: MirrordPolicy,
    service_a_can_steal: bool,
    service_b_can_steal: bool,
}

// TODO: test policies:
//  - with/out path pattern
//  - with/out label selector
//  - all steal / unfiltered steal
//  - deployment target
//  With every policy - check that stealing is blocked in selected target and not blocked in
//  other target.

/// Two services with non-random names, in the same random namespace.
/// Using random namespace to isolate tests and separate policies (not have policies apply to other
/// tests).
#[fixture]
pub async fn services(
    #[future] kube_client: kube::Client,
    #[future]
    #[from(kube_client)]
    another_kube_client: kube::Client,
) -> (KubeService, KubeService) {
    let namespace = format!("e2e-tests-policies-{}", crate::utils::random_string());
    (
        service(
            &namespace,
            "NodePort",
            "ghcr.io/metalbear-co/mirrord-pytest:latest",
            "policy-e2e-test-service-a",
            false,
            kube_client,
        )
        .await,
        service(
            &namespace,
            "NodePort",
            "ghcr.io/metalbear-co/mirrord-pytest:latest",
            "policy-e2e-test-service-b",
            false,
            another_kube_client,
        )
        .await,
    )
}

/// Block all stealing for all targets, should not work for either service.
fn block_steal_without_qualifiers() -> PolicyTestCase {
    PolicyTestCase {
        policy: MirrordPolicy::new(
            "e2e-test-block-steal-without-qualifiers",
            MirrordPolicySpec {
                target_path: None,
                selector: None,
                block: vec![BlockedFeature::Steal],
            },
        ),
        service_b_can_steal: false,
        service_a_can_steal: false,
    }
}

/// Block all stealing for all targets, should not work for either service.
fn block_steal_with_path_pattern() -> PolicyTestCase {
    PolicyTestCase {
        policy: MirrordPolicy::new(
            "e2e-test-block-steal-with-path-pattern",
            MirrordPolicySpec {
                target_path: Some("*-service-a*".into()),
                selector: None,
                block: vec![BlockedFeature::Steal],
            },
        ),
        service_b_can_steal: true,
        service_a_can_steal: false,
    }
}

/// Assert that stealing failed due to policy if `expected_success` else succeeded.
/// This function should be timed out - caller's responsibility.
async fn assert_steal_result(mut test_proc: TestProcess, expected_success: bool) {
    if expected_success {
        test_proc
            .wait_for_line(Duration::from_secs(40), "daemon subscribed")
            .await;
        test_proc.child.kill().await.unwrap()
    } else {
        test_proc
            .wait_for_line(Duration::from_secs(40), "forbidden by the mirrord policy")
            .await;
        // Make sure process exits on a policy block. (test should have a timeout).
        test_proc.wait_assert_fail().await
    }
}

/// Run mirrord, verify stealing fails if blocked, succeeds if not.
/// This function should be timed out - caller's responsibility.
async fn run_mirrord_and_verify_steal_result(kube_service: &KubeService, should_succeed: bool) {
    let application = Application::NodeHTTP;

    let test_proc = application
        .run(
            &kube_service.target,
            Some(&kube_service.namespace),
            Some(vec!["--steal", "--fs-mode=local"]),
            None,
        )
        .await;

    assert_steal_result(test_proc, should_succeed).await;
}

/// Set a policy, try to steal, verify that stealing fails with the target for which it should be
/// blocked and succeeds with the target for which it should not be blocked.
#[rstest]
#[case(block_steal_without_qualifiers())]
#[case(block_steal_with_path_pattern())]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(60))]
pub async fn create_policy_and_try_to_steal(
    #[future] services: (KubeService, KubeService),
    #[future] kube_client: kube::Client,
    #[case] test_case: PolicyTestCase,
) {
    let kube_client = kube_client.await;
    let (service_a, service_b) = services.await;

    // Create policy, delete it when test exits.
    let _policy_guard =
        PolicyGuard::new(kube_client, &test_case.policy, &service_a.namespace).await;

    println!("Running mirrord against service a");
    run_mirrord_and_verify_steal_result(&service_a, test_case.service_a_can_steal).await;
    println!("Running mirrord against service b");
    run_mirrord_and_verify_steal_result(&service_b, test_case.service_b_can_steal).await;
}
