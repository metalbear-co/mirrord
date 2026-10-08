//! End-to-end tests of `mirrord operator install` and `mirrord operator uninstall`.
//!
//! The operator that these tests install is cluster-wide, and every mirrord session in the
//! cluster finds and uses it. So only the `operator-install` nextest profile runs these tests, one
//! at a time and without other tests (see `.config/nextest.toml`). Each test first removes what an
//! earlier test could have left in the cluster, and a test that fails removes what it created (see
//! [`ResetOnFailure`]).
//!
//! The operator gets an API key that the MetalBear cloud rejects, so that no test starts a trial.
//! Without a license, the operator still serves its status.
#![cfg(test)]

use std::{collections::HashMap, ops::Not, process::ExitStatus, time::Duration};

use futures::future::join_all;
use k8s_openapi::api::{
    admissionregistration::v1::{ValidatingAdmissionPolicy, ValidatingAdmissionPolicyBinding},
    core::v1::Pod,
};
use kube::{
    api::{DeleteParams, DynamicObject, ListParams, PostParams},
    discovery::{verbs, Discovery},
    Api, Client, Config, ResourceExt,
};
use mirrord_operator::crd::{MirrordOperatorCrd, OPERATOR_STATUS_NAME};
use mirrord_test_utils::run_command::run_mirrord;
use rstest::rstest;
use tempfile::TempDir;
use tokio::process::Command;

use crate::utils::client::{kube_client, KubeClient};

const API_KEY: &str = "mirrord-e2e-tests-invalid-api-key";

/// The helm release that `mirrord operator install` attributes all objects to.
const RELEASE_NAME: &str = "mirrord-operator";

/// Has [`RELEASE_NAME`] on every object that the installation creates.
const RELEASE_NAME_ANNOTATION: &str = "meta.helm.sh/release-name";

/// The API group that the operator serves itself. Its objects are not stored in the cluster, and
/// the group is not available while the operator is down.
const OPERATOR_API_GROUP: &str = "operator.metalbear.co";

/// The label that selects the operator pods.
const OPERATOR_POD_LABEL: (&str, &str) = ("app", "mirrord-operator");

/// Name of the admission policy and its binding that stop the operator pods from being created.
const DENY_OPERATOR_PODS: &str = "mirrord-e2e-deny-operator-pods";

/// The message of the admission policy when it denies a pod.
const DENIED_MESSAGE: &str = "denied by the e2e tests";

/// Lets the install that cannot succeed fail after it checks the operator one time.
const SHORT_READY_TIMEOUT: &str = "0s";

/// The result of a `mirrord operator` command that has finished.
struct Output {
    status: ExitStatus,
    stdout: String,
    /// With all whitespace changed to single spaces, since the error report is wrapped to the
    /// width of a terminal.
    stderr: String,
}

impl Output {
    fn assert_success(&self) {
        assert!(
            self.status.success(),
            "the command failed with {}: {}",
            self.status,
            self.stderr
        );
    }
}

async fn mirrord_operator(args: &[&str]) -> Output {
    let env = HashMap::from([
        ("MIRRORD_TELEMETRY", "false"),
        ("MIRRORD_PROGRESS_MODE", "off"),
        ("MIRRORD_CHECK_VERSION", "false"),
    ]);

    let mut process = run_mirrord([&["operator"], args].concat(), env, None).await;
    let status = process.wait().await;
    let stderr = process.get_stderr().await;

    Output {
        status,
        stdout: process.get_stdout().await,
        stderr: stderr.split_whitespace().collect::<Vec<_>>().join(" "),
    }
}

async fn install(args: &[&str]) -> Output {
    mirrord_operator(&[&["install", "--api-key", API_KEY, "--yes"], args].concat()).await
}

async fn uninstall() -> Output {
    mirrord_operator(&["uninstall", "--yes"]).await
}

/// Describes each object in the cluster that belongs to the [`RELEASE_NAME`] helm release, which
/// are the objects that the installation created.
async fn release_objects(client: &Client) -> Vec<String> {
    let discovery = Discovery::new(client.clone())
        .exclude(&[OPERATOR_API_GROUP])
        .run_aggregated()
        .await
        .unwrap();

    let lists = discovery
        .groups()
        .flat_map(|group| group.recommended_resources())
        .filter(|(_, capabilities)| capabilities.supports_operation(verbs::LIST))
        .map(|(resource, _)| async move {
            let list = match Api::<DynamicObject>::all_with(client.clone(), &resource)
                .list_metadata(&ListParams::default())
                .await
            {
                Ok(list) => list,
                // Discovery can still list the resources of a CRD that was just deleted.
                Err(kube::Error::Api(status)) if status.code == 404 => return Vec::new(),
                Err(error) => panic!("failed to list {}: {error}", resource.plural),
            };

            list.items
                .into_iter()
                .filter(|object| {
                    object
                        .annotations()
                        .get(RELEASE_NAME_ANNOTATION)
                        .is_some_and(|release| release == RELEASE_NAME)
                })
                .map(|object| {
                    format!(
                        "{} {}/{}",
                        resource.kind,
                        object.namespace().unwrap_or_default(),
                        object.name_any()
                    )
                })
                .collect::<Vec<_>>()
        });

    join_all(lists).await.concat()
}

async fn operator_status(client: &Client) -> kube::Result<MirrordOperatorCrd> {
    Api::<MirrordOperatorCrd>::all(client.clone())
        .get(OPERATOR_STATUS_NAME)
        .await
}

/// Runs `program`, which can run helm, with the helm configuration, cache and data in a temporary
/// directory, so that `helm repo add` does not change the helm repositories of the user.
async fn run_with_helm(program: &str, args: &[&str]) {
    let helm_home = TempDir::new().unwrap();
    let output = Command::new(program)
        .args(args)
        .env("HELM_CONFIG_HOME", helm_home.path().join("config"))
        .env("HELM_CACHE_HOME", helm_home.path().join("cache"))
        .env("HELM_DATA_HOME", helm_home.path().join("data"))
        .output()
        .await
        .unwrap();
    assert!(
        output.status.success(),
        "`{program} {}` failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Removes everything that a test can create in the cluster.
async fn reset(client: &Client) {
    allow_operator_pods(client).await;
    run_with_helm(
        "helm",
        &["uninstall", RELEASE_NAME, "--ignore-not-found", "--wait"],
    )
    .await;
    uninstall().await.assert_success();
}

/// Runs [`reset`] when the test fails, so that the operator and the admission policy do not stay
/// in the cluster, where they make all mirrord sessions fail. Does not keep them for
/// [`PRESERVE_FAILED_ENV_NAME`](crate::utils::PRESERVE_FAILED_ENV_NAME), since the next test,
/// or the next try of this test, removes them when it starts.
///
/// The cluster is reset in a new runtime, since the runtime of the test is blocked while this is
/// dropped.
struct ResetOnFailure(Config);

impl Drop for ResetOnFailure {
    fn drop(&mut self) {
        if std::thread::panicking().not() {
            return;
        }

        let config = self.0.clone();
        let _ = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("failed to create a tokio runtime")
                .block_on(async { reset(&Client::try_from(config).unwrap()).await })
        })
        .join();
    }
}

/// Resets the cluster for a new test.
async fn start_test(kube_client: KubeClient) -> (Client, ResetOnFailure) {
    let client = kube_client.get_client();
    reset(&client).await;

    (client, ResetOnFailure(kube_client.get_config()))
}

/// Makes the cluster reject new operator pods, so that the operator can't become ready.
///
/// Returns when the cluster enforces the policy, which it does not do right after the policy is
/// created.
async fn deny_operator_pods(client: &Client) {
    let (label, value) = OPERATOR_POD_LABEL;
    let policy = serde_json::from_value::<ValidatingAdmissionPolicy>(serde_json::json!({
        "metadata": { "name": DENY_OPERATOR_PODS },
        "spec": {
            "failurePolicy": "Fail",
            "matchConstraints": {
                "objectSelector": { "matchLabels": { label: value } },
                "resourceRules": [{
                    "apiGroups": [""],
                    "apiVersions": ["v1"],
                    "operations": ["CREATE"],
                    "resources": ["pods"],
                }],
            },
            "validations": [{ "expression": "false", "message": DENIED_MESSAGE }],
        },
    }))
    .unwrap();
    let binding = serde_json::from_value::<ValidatingAdmissionPolicyBinding>(serde_json::json!({
        "metadata": { "name": DENY_OPERATOR_PODS },
        "spec": { "policyName": DENY_OPERATOR_PODS, "validationActions": ["Deny"] },
    }))
    .unwrap();
    Api::all(client.clone())
        .create(&PostParams::default(), &policy)
        .await
        .unwrap();
    Api::all(client.clone())
        .create(&PostParams::default(), &binding)
        .await
        .unwrap();

    let pod = serde_json::from_value::<Pod>(serde_json::json!({
        "metadata": { "name": "mirrord-e2e-denied-pod", "labels": { label: value } },
        "spec": { "containers": [{ "name": "main", "image": "busybox" }] },
    }))
    .unwrap();
    let dry_run = PostParams {
        dry_run: true,
        field_manager: None,
    };
    let pods = Api::<Pod>::default_namespaced(client.clone());
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            match pods.create(&dry_run, &pod).await {
                Ok(_) => tokio::time::sleep(Duration::from_millis(200)).await,
                Err(kube::Error::Api(status)) if status.message.contains(DENIED_MESSAGE) => {
                    return;
                }
                Err(error) => panic!("failed to check the admission policy: {error}"),
            }
        }
    })
    .await
    .expect("the cluster did not enforce the admission policy in time");
}

async fn allow_operator_pods(client: &Client) {
    for result in [
        Api::<ValidatingAdmissionPolicyBinding>::all(client.clone())
            .delete(DENY_OPERATOR_PODS, &DeleteParams::default())
            .await
            .map(drop),
        Api::<ValidatingAdmissionPolicy>::all(client.clone())
            .delete(DENY_OPERATOR_PODS, &DeleteParams::default())
            .await
            .map(drop),
    ] {
        match result {
            Ok(()) => {}
            Err(kube::Error::Api(status)) if status.code == 404 => {}
            Err(error) => panic!("failed to delete the admission policy: {error}"),
        }
    }
}

#[rstest]
#[tokio::test]
async fn uninstall_removes_installation(#[future] kube_client: KubeClient) {
    let (client, _reset) = start_test(kube_client.await).await;

    install(&[]).await.assert_success();
    operator_status(&client).await.unwrap();
    assert!(release_objects(&client).await.is_empty().not());

    uninstall().await.assert_success();
    let leftovers = release_objects(&client).await;
    assert!(leftovers.is_empty(), "{leftovers:?}");

    install(&[]).await.assert_success();
    operator_status(&client).await.unwrap();

    uninstall().await.assert_success();
}

#[rstest]
#[tokio::test]
async fn uninstall_removes_failed_installation(#[future] kube_client: KubeClient) {
    let (client, _reset) = start_test(kube_client.await).await;
    deny_operator_pods(&client).await;

    let output = install(&["--ready-timeout", SHORT_READY_TIMEOUT]).await;
    assert!(output.status.success().not());
    let error = format!("the operator did not become ready within {SHORT_READY_TIMEOUT}");
    assert!(output.stderr.contains(&error), "{}", output.stderr);
    assert!(release_objects(&client).await.is_empty().not());

    uninstall().await.assert_success();
    let leftovers = release_objects(&client).await;
    assert!(leftovers.is_empty(), "{leftovers:?}");

    allow_operator_pods(&client).await;
    install(&[]).await.assert_success();
    operator_status(&client).await.unwrap();

    uninstall().await.assert_success();
}

#[rstest]
#[tokio::test]
async fn install_refuses_registered_operator(#[future] kube_client: KubeClient) {
    let (client, _reset) = start_test(kube_client.await).await;

    install(&[]).await.assert_success();
    let version = operator_status(&client)
        .await
        .unwrap()
        .spec
        .operator_version;

    let output = install(&[]).await;
    assert!(output.status.success().not());
    let error = format!("mirrord operator {version} is already installed in namespace `mirrord`");
    assert!(output.stderr.contains(&error), "{}", output.stderr);

    uninstall().await.assert_success();
}

#[rstest]
#[tokio::test]
async fn helm_adopts_installation(#[future] kube_client: KubeClient) {
    let (client, _reset) = start_test(kube_client.await).await;

    let output = install(&[]).await;
    output.assert_success();
    let commands = output
        .stdout
        .find("helm repo add")
        .map(|start| &output.stdout[start..])
        .unwrap_or_else(|| panic!("no helm commands in the output: {}", output.stdout));

    run_with_helm("sh", &["-ec", commands]).await;
    operator_status(&client).await.unwrap();

    // helm only removes the objects of its release, so nothing is left if it adopted them all.
    run_with_helm("helm", &["uninstall", RELEASE_NAME, "--wait"]).await;
    let leftovers = release_objects(&client).await;
    assert!(leftovers.is_empty(), "{leftovers:?}");
}
