#![cfg(test)]
#![cfg(all(not(feature = "operator"), feature = "job"))]
//! Test that the agent can successfully detect and exit on dirty IP tables in the target.

use std::time::Duration;

use k8s_openapi::api::core::v1::{Namespace, Pod};
use kube::{api::PostParams, Api, Client};
use rstest::*;
use serde_json::json;

use crate::utils::{
    kube_client, random_string, watch, EnvApp, ResourceGuard, PRESERVE_FAILED_ENV_NAME,
    TEST_RESOURCE_LABEL,
};

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
#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(120))]
pub async fn agent_exits_on_dirty_tables(
    #[values(EnvApp::NodeInclude)] application: EnvApp,
    #[future] kube_client: Client,
) {
    use crate::utils::run_exec_with_target;

    let kube_client = kube_client.await;
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
    .expect(format!("Should be able to create namespace {namespace}").as_str());

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
                            "command": ["sh", "-c", "iptables -t nat -N MIRRORD_INPUT"],
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
                            "ports": [ { "containerPort": 80 } ]
                        }
                    ]
                }
            }))
            .unwrap(),
        )
        .await
        .expect(
            format!("Should be able to create pod {pod_name} in namespace {namespace}").as_str(),
        );
    println!("Created pod `{pod_name}`");

    // Use the pod as a target, which has an init container that inserts a mirrord chain name
    let target = format!("pod/{}", pod_name);

    // Wait for the target pod to be ready
    println!("Waiting for target `{target}` in namespace `{namespace}` to be ready...");
    watch::wait_until_pod_ready(&pod_name, &namespace, kube_client).await;

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
