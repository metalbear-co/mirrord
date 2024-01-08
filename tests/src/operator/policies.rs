#![cfg(test)]
#![cfg(feature = "operator")]
//! Test that mirrordpolicies work and features are blocked.

use std::{collections::BTreeMap, time::Duration};

use kube::Api;
use mirrord_operator::crd::{
    label_selector::LabelSelector, BlockedFeature, MirrordPolicy, MirrordPolicySpec,
};
use rstest::{fixture, rstest};

use crate::utils::{
    config_dir, kube_client, service, Application, KubeService, ResourceGuard, TestProcess,
};

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

enum CanSteal {
    No,
    OnlyWithFilter,
    EvenWithoutFilter,
}

impl CanSteal {
    fn should_work_without_filter(&self) -> bool {
        matches!(self, EvenWithoutFilter)
    }

    fn should_work_with_filter(&self) -> bool {
        matches!(self, EvenWithoutFilter | OnlyWithFilter)
    }
}

use CanSteal::*;

struct PolicyTestCase {
    policy: MirrordPolicy,
    service_a_can_steal: CanSteal,
    service_b_can_steal: CanSteal,

    /// Use the deployment as a target?
    target_deploy_a: bool,
    target_deploy_b: bool,
}

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

/// Block all stealing for all targets, stealing should not work for either service.
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
        service_b_can_steal: No,
        service_a_can_steal: No,
        target_deploy_a: false,
        target_deploy_b: false,
    }
}

/// Block all stealing only for service a by path, stealing should work for service b.
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
        service_b_can_steal: EvenWithoutFilter,
        service_a_can_steal: No,
        target_deploy_a: false,
        target_deploy_b: false,
    }
}

/// Block unfiltered stealing only for service a by path.
fn block_unfiltered_steal_with_path_pattern() -> PolicyTestCase {
    PolicyTestCase {
        policy: MirrordPolicy::new(
            "e2e-test-block-unfiltered-steal-with-path-pattern",
            MirrordPolicySpec {
                target_path: Some("*-service-a*".into()),
                selector: None,
                block: vec![BlockedFeature::StealWithoutFilter],
            },
        ),
        service_b_can_steal: EvenWithoutFilter,
        service_a_can_steal: OnlyWithFilter,
        target_deploy_a: false,
        target_deploy_b: false,
    }
}

/// Block unfiltered stealing only for deployment a by path.
fn block_unfiltered_steal_with_deployment_path_pattern() -> PolicyTestCase {
    PolicyTestCase {
        policy: MirrordPolicy::new(
            "e2e-test-block-unfiltered-steal-with-path-pattern",
            MirrordPolicySpec {
                target_path: Some("deploy/*service-a*".into()),
                selector: None,
                block: vec![BlockedFeature::StealWithoutFilter],
            },
        ),
        service_a_can_steal: OnlyWithFilter,
        service_b_can_steal: EvenWithoutFilter,
        target_deploy_a: true,
        target_deploy_b: false,
    }
}

/// Block all stealing only for service a by label selector, stealing should work for service b.
fn block_steal_with_label_selector() -> PolicyTestCase {
    PolicyTestCase {
        policy: MirrordPolicy::new(
            "e2e-test-block-steal-with-label-selector",
            MirrordPolicySpec {
                target_path: None,
                selector: Some(LabelSelector {
                    match_expressions: None,
                    match_labels: Some(BTreeMap::from([(
                        "test-label-for-pods".to_string(),
                        "pod-policy-e2e-test-service-a".to_string(),
                    )])),
                }),
                block: vec![BlockedFeature::Steal],
            },
        ),
        service_b_can_steal: EvenWithoutFilter,
        service_a_can_steal: No,
        target_deploy_a: false,
        target_deploy_b: false,
    }
}

/// Block all stealing in a policy where the path matches only service a but the labels match only
/// service b. Stealing should work for both, as the policy should not match either.
fn block_steal_with_unmatching_policy() -> PolicyTestCase {
    PolicyTestCase {
        policy: MirrordPolicy::new(
            "e2e-test-block-steal-with-unmatching-policy",
            MirrordPolicySpec {
                target_path: Some("*service-b*".to_string()),
                selector: Some(LabelSelector {
                    match_expressions: None,
                    match_labels: Some(BTreeMap::from([(
                        "test-label-for-pods".to_string(),
                        "pod-service-b".to_string(),
                    )])),
                }),
                block: vec![BlockedFeature::Steal],
            },
        ),
        service_b_can_steal: EvenWithoutFilter,
        service_a_can_steal: EvenWithoutFilter,
        target_deploy_a: false,
        target_deploy_b: false,
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

/// Run mirrord twice, once without an http filter and once with, and verify it works or fails in
/// each run according to `can_steal`. This function should be timed out - caller's responsibility.
async fn run_mirrord_and_verify_steal_result(
    kube_service: &KubeService,
    target_deployment: bool,
    can_steal: CanSteal,
) {
    let application = Application::Go21HTTP;

    let target = if target_deployment {
        format!("deploy/{}", kube_service.name)
    } else {
        kube_service.target.clone()
    };

    let test_proc = application
        .run(
            &target,
            Some(&kube_service.namespace),
            Some(vec!["--steal", "--fs-mode=local"]),
            None,
        )
        .await;

    assert_steal_result(test_proc, can_steal.should_work_without_filter()).await;

    let mut config_path = config_dir().clone();
    config_path.push("http_filter_header.json");

    let test_proc = application
        .run(
            &kube_service.target,
            Some(&kube_service.namespace),
            Some(vec!["--config-file", config_path.to_str().unwrap()]),
            None,
        )
        .await;

    assert_steal_result(test_proc, can_steal.should_work_with_filter()).await;
}

/// Set a policy, try to steal both with and without a filter from two services, verify subscribing
/// fails (only) where it is supposed to.
#[rstest]
#[case(block_steal_without_qualifiers())]
#[case(block_steal_with_path_pattern())]
#[case(block_steal_with_label_selector())]
#[case(block_steal_with_unmatching_policy())]
#[case(block_unfiltered_steal_with_path_pattern())]
#[case(block_unfiltered_steal_with_deployment_path_pattern())]
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
    run_mirrord_and_verify_steal_result(
        &service_a,
        test_case.target_deploy_a,
        test_case.service_a_can_steal,
    )
    .await;
    println!("Running mirrord against service b");
    run_mirrord_and_verify_steal_result(
        &service_b,
        test_case.target_deploy_b,
        test_case.service_b_can_steal,
    )
    .await;
}
