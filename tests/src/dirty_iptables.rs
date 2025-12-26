#![cfg(test)]
#![cfg(all(not(feature = "operator"), feature = "job"))]
//! Test that the agent can successfully detect and exit on dirty IP tables in the target.

use std::time::Duration;

use k8s_openapi::api::core::v1::{Namespace, Pod};
use kube::{api::PostParams, Api, Client};
use rstest::*;
use serde_json::json;

use crate::utils::{
    application::env::EnvApp, kube_client, operator_installed, random_string,
    resource_guard::ResourceGuard, run_command::run_exec_with_target, watch,
    PRESERVE_FAILED_ENV_NAME, TEST_RESOURCE_LABEL,
};

struct DirtyIptablesTest {
    namespace: String,
    target: String,
    kube_client: Client,
}

/// This fixture only creates resources if a mirrord operator is NOT installed.
#[fixture]
async fn oss_only_dirty_iptables_test(#[future] kube_client: Client) -> Option<DirtyIptablesTest> {
    let kube_client = kube_client.await;
    if operator_installed(&kube_client).await.unwrap() {
        return None;
    }
    Some(dirty_iptables_test_inner(kube_client).await)
}

#[fixture]
async fn dirty_iptables_test(#[future] kube_client: Client) -> DirtyIptablesTest {
    let kube_client = kube_client.await;
    dirty_iptables_test_inner(kube_client).await
}

async fn dirty_iptables_test_inner(kube_client: Client) -> DirtyIptablesTest {
    let namespace = format!("dirty-iptables-{}", random_string());
    let pod_name = format!("{namespace}-pod");
    let init_image = std::env::var("MIRRORD_AGENT_IMAGE").unwrap_or_else(|_| "test".to_string());
    let delete_after_fail = std::env::var_os(PRESERVE_FAILED_ENV_NAME).is_none();

    // Create a namespace and a pod to target and check for pre-existing mirrord chain names
    // Create the namespace and wrap it in ResourceGuard
    let namespace_api: Api<Namespace> = Api::all(kube_client.clone());
    let _namespace_guard = ResourceGuard::create::<Namespace>(
        namespace_api.clone(),
        &serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Namespace",
            "metadata": {
                "name": &namespace,
                "labels": {
                    TEST_RESOURCE_LABEL.0: TEST_RESOURCE_LABEL.1,
                }
            },
        }))
        .unwrap(),
        delete_after_fail,
    )
    .await
    .unwrap_or_else(|error| panic!("Should be able to create namespace {namespace}: {error}"));

    // Create a single pod as the target and wrap it in ResourceGuard
    let pod_api: Api<Pod> = Api::namespaced(kube_client.clone(), &namespace);
    let _pod = pod_api
        .create(
            &PostParams::default(),
            &serde_json::from_value(json!({
                "apiVersion": "v1",
                "kind": "Pod",
                "metadata": {
                    "name": pod_name,
                    "namespace": &namespace,
                    "labels": {
                        TEST_RESOURCE_LABEL.0: TEST_RESOURCE_LABEL.1,
                    }
                },
                "spec": {
                    "initContainers": [
                        {
                            "name": "init-bad-cleanup",
                            "image": init_image,
                            "imagePullPolicy": "Never",
                            "command": ["/usr/sbin/iptables-legacy", "-t", "nat", "-N", "MIRRORD_INPUT"],
                            "securityContext": {
                                "capabilities": {
                                    "add": ["CAP_NET_ADMIN"]
                                },
                                "privileged": true,
                            }
                        }
                    ],
                    "containers": [
                        {
                            "name": "py-serv",
                            "image": "ghcr.io/metalbear-co/mirrord-pytest:latest",
                            "ports": [ { "containerPort": 80 } ],
                            "env": [
                                {"name": "MIRRORD_FAKE_VAR_FIRST", "value": "mirrord.is.running"},
                                {"name": "MIRRORD_FAKE_VAR_SECOND", "value": "7777"}
                            ]
                        }
                    ]
                }
            }))
                .unwrap(),
        )
        .await
        .unwrap_or_else(|error| {
            panic!("Should be able to create pod {pod_name} in namespace {namespace}: {error}")
        });
    println!("Created pod `{pod_name}`");

    // Use the pod as a target, which has an init container that inserts a mirrord chain name
    let target = format!("pod/{}", pod_name);

    // Wait for the target pod to be ready
    println!("Waiting for target `{target}` in namespace `{namespace}` to be ready...");
    watch::wait_until_pod_ready(&pod_name, &namespace, kube_client.clone()).await;

    DirtyIptablesTest {
        namespace,
        target,
        kube_client,
    }
}

/// Starts mirrord with a target that has an existing mirrord IP table chain from another agent or a
/// failed cleanup. This should cause mirrord to exit and NOT perform cleanup on the table. This
/// behaviour is required in order to have static chain names in the agent.
///
/// The existing mirrord IP table chain name is introduced via init container before the target
/// starts. Instead of checking for the existence of the chain name after a run (to ensure it was
/// not cleaned up), we can run the agent a second time and expect the same exit behaviour as the
/// first run.
///
/// #### RELEVANT CODE
/// Function that performs check: `SafeIpTables::ensure_iptables_clean()`
/// Static chain/ table names: `IPTABLE_PREROUTING`, `IPTABLE_MESH`, `IPTABLE_STANDARD`,
/// `IPTABLES_TABLE_NAME`
#[cfg_attr(target_os = "windows", ignore)]
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(120))]
pub async fn agent_exits_on_dirty_tables(
    #[future] oss_only_dirty_iptables_test: Option<DirtyIptablesTest>,
) {
    let Some(DirtyIptablesTest {
        namespace, target, ..
    }) = oss_only_dirty_iptables_test.await
    else {
        return;
    };
    let application = EnvApp::NodeInclude;
    let mirrord_args = Some(application.mirrord_args().unwrap_or_default());
    let mut process = run_exec_with_target(
        application.command(),
        &target,
        Some(&namespace),
        mirrord_args.clone(),
        None,
    )
    .await;

    // check for expected error message in output
    process.wait_assert_fail().await;
    process
        .assert_stderr_contains("Detected dirty iptables.")
        .await;

    // // Run agent a second time to ensure no cleanup occurred
    let mut process = run_exec_with_target(
        application.command(),
        &target,
        Some(&namespace),
        mirrord_args,
        None,
    )
    .await;

    // check for expected error message in output
    process.wait_assert_fail().await;
    process
        .assert_stderr_contains("Detected dirty iptables.")
        .await;
}

/// Verify that when the `agent.clean_iptables_on_start` option is set, the agent cleans up the
/// iptables and starts, instead of erroring out.
#[cfg_attr(target_os = "windows", ignore)]
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(120))]
pub async fn agent_cleans_up_and_starts_on_dirty_tables(
    #[future] dirty_iptables_test: DirtyIptablesTest,
) {
    let DirtyIptablesTest {
        namespace,
        target,
        kube_client,
    } = dirty_iptables_test.await;
    let env = if operator_installed(&kube_client).await.unwrap() {
        // if the operator is installed, cleanup should be enabled by default.
        None
    } else {
        Some(vec![("MIRRORD_AGENT_CLEAN_IPTABLES_ON_START", "true")])
    };
    let application = EnvApp::NodeInclude;
    let mirrord_args = Some(application.mirrord_args().unwrap_or_default());
    let mut process = run_exec_with_target(
        application.command(),
        &target,
        Some(&namespace),
        mirrord_args.clone(),
        env,
    )
    .await;

    process.wait_assert_success().await;
}
