#![allow(dead_code)]
use cluster_resource::*;
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{ConfigMap, EnvFromSource, Namespace, Service},
};
use kube::{api::DeleteParams, Api, Client, Resource, ResourceExt};
use kube_service::KubeService;
use mirrord_kube::api::kubernetes::rollout::Rollout;
use resource_guard::ResourceGuard;
use rstest::*;
use serde_json::{json, Value};

use super::{cluster_resource, kube_service, resource_guard};
use crate::utils::{
    default_env, format_time, kube_client, random_string, set_ipv6_only, watch,
    PRESERVE_FAILED_ENV_NAME, TEST_RESOURCE_LABEL,
};

pub(crate) mod operator;

/// Create a new [`KubeService`] and related Kubernetes resources. The resources will be deleted
/// when the returned service is dropped, unless it is dropped during panic.
/// This behavior can be changed, see [`PRESERVE_FAILED_ENV_NAME`].
/// * `randomize_name` - whether a random suffix should be added to the end of the resource names
#[fixture]
pub async fn basic_service(
    #[default("default")] namespace: &str,
    #[default("NodePort")] service_type: &str,
    #[default("ghcr.io/metalbear-co/mirrord-pytest:latest")] image: &str,
    #[default("http-echo")] service_name: &str,
    #[default(true)] randomize_name: bool,
    #[future] kube_client: Client,
) -> KubeService {
    internal_service(
        namespace,
        service_type,
        image,
        service_name,
        randomize_name,
        kube_client.await,
        default_env(),
        None,
        None,
        false,
        TestWorkloadType::Deployment,
    )
    .await
}

/// Create a new [`KubeService`] and related Kubernetes resources.
///
/// The resources will be deleted
/// when the returned service is dropped, unless it is dropped during panic.
/// This behavior can be changed, see [`PRESERVE_FAILED_ENV_NAME`].
/// * `randomize_name` - whether a random suffix should be added to the end of the resource names
/// * `env` - `Value`, should be `Value::Array` of kubernetes container env var definitions.
pub async fn service_with_env(
    namespace: &str,
    service_type: &str,
    image: &str,
    service_name: &str,
    randomize_name: bool,
    kube_client: Client,
    env: Value,
) -> KubeService {
    internal_service(
        namespace,
        service_type,
        image,
        service_name,
        randomize_name,
        kube_client,
        env,
        None,
        None,
        false,
        TestWorkloadType::Deployment,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn service_with_env_and_env_from(
    namespace: &str,
    service_type: &str,
    image: &str,
    service_name: &str,
    randomize_name: bool,
    kube_client: Client,
    env: Value,
    env_from: Option<Vec<EnvFromSource>>,
    config_maps: Option<Vec<ConfigMap>>,
) -> KubeService {
    internal_service(
        namespace,
        service_type,
        image,
        service_name,
        randomize_name,
        kube_client,
        env,
        env_from,
        config_maps,
        false,
        TestWorkloadType::Deployment,
    )
    .await
}

#[derive(Clone, Copy, Default)]
pub enum TestWorkloadType {
    #[default]
    Deployment,
    ArgoRolloutWithWorkloadRef,
    ArgoRolloutWithTemplate,
}

impl TestWorkloadType {
    pub fn is_rollout(&self) -> bool {
        matches!(
            self,
            TestWorkloadType::ArgoRolloutWithTemplate
                | TestWorkloadType::ArgoRolloutWithWorkloadRef
        )
    }
}

async fn create_rollout(
    rollout_api: Api<Rollout>,
    rollout: &Rollout,
    delete_after_fail: bool,
    guards: &mut Vec<ResourceGuard>,
    name: &str,
    namespace: &str,
    kube_client: &Client,
) {
    let (rollout_guard, rollout) = ResourceGuard::create(rollout_api, rollout, delete_after_fail)
        .await
        .unwrap_or_else(|err| {
            panic!(
                "Failed to create rollout guard! Error: \n{err:?}\nRollout:\n{}",
                serde_json::to_string_pretty(&rollout).unwrap()
            )
        });
    println!(
        "Created rollout\n{}",
        serde_json::to_string_pretty(&rollout).unwrap()
    );
    guards.push(rollout_guard);

    // Wait for the rollout to have at least 1 available replica
    watch::wait_until_rollout_available(name, namespace, 1, kube_client.clone()).await;
}

/// Internal function to create a custom [`KubeService`].
/// We keep this private so that whenever we need more customization of test resources, we can
/// change this function and how the public ones use it, and add a new public function that exposes
/// more customization, and we don't need to change all existing usages of public functions/fixtures
/// in tests.
///
/// Create a new [`KubeService`] and related Kubernetes resources. The resources will be
/// deleted when the returned service is dropped, unless it is dropped during panic.
/// This behavior can be changed, see [`PRESERVE_FAILED_ENV_NAME`].
/// * `randomize_name` - whether a random suffix should be added to the end of the resource names
/// * `env` - `Value`, should be `Value::Array` of kubernetes container env var definitions.
#[allow(clippy::too_many_arguments)]
pub async fn internal_service(
    namespace: &str,
    service_type: &str,
    image: &str,
    service_name: &str,
    randomize_name: bool,
    kube_client: Client,
    env: Value,
    env_from: Option<Vec<EnvFromSource>>,
    config_maps: Option<Vec<ConfigMap>>,
    ipv6_only: bool,
    workload_type: TestWorkloadType,
) -> KubeService {
    let delete_after_fail = std::env::var_os(PRESERVE_FAILED_ENV_NAME).is_none();

    let namespace_api: Api<Namespace> = Api::all(kube_client.clone());
    let deployment_api: Api<Deployment> = Api::namespaced(kube_client.clone(), namespace);
    let rollout_api: Api<Rollout> = Api::namespaced(kube_client.clone(), namespace);
    let service_api: Api<Service> = Api::namespaced(kube_client.clone(), namespace);
    let mut guards = Vec::with_capacity(4);

    let name = if randomize_name {
        format!("{}-{}", service_name, random_string())
    } else {
        // If using a non-random name, delete existing resources first.
        // Just continue if they don't exist.
        // Force delete
        let delete_params = DeleteParams {
            grace_period_seconds: Some(0),
            ..Default::default()
        };

        let _ = service_api.delete(service_name, &delete_params).await;
        let _ = deployment_api.delete(service_name, &delete_params).await;
        let _ = rollout_api.delete(service_name, &delete_params).await;

        service_name.to_string()
    };

    println!(
        "{} creating service {name} in namespace {namespace}",
        format_time()
    );

    // Create namespace and wrap it in ResourceGuard if it does not yet exist.
    if let Ok((guard, _namespace_resource)) = ResourceGuard::create::<Namespace>(
        namespace_api.clone(),
        &serde_json::from_value(json!({
            "apiVersion": "v1",
            "kind": "Namespace",
            "metadata": {
                "name": namespace,
                "labels": {
                    TEST_RESOURCE_LABEL.0: TEST_RESOURCE_LABEL.1,
                }
            },
        }))
        .unwrap(),
        delete_after_fail,
    )
    .await
    {
        guards.push(guard);
    }

    if let Some(config_maps) = config_maps {
        let api = Api::<ConfigMap>::namespaced(kube_client.clone(), namespace);
        for map in config_maps {
            println!(
                "creating {} {} in namespace {}",
                ConfigMap::kind(&()),
                map.name_any(),
                namespace
            );
            api.create(&Default::default(), &map).await.unwrap();
        }
    }

    match workload_type {
        TestWorkloadType::Deployment => {
            let deployment = deployment_from_json(&name, image, env, env_from, 1);
            let (deployment_guard, _deployment) =
                ResourceGuard::create(deployment_api.clone(), &deployment, delete_after_fail)
                    .await
                    .unwrap();
            guards.push(deployment_guard);
        }
        TestWorkloadType::ArgoRolloutWithWorkloadRef => {
            let deployment = deployment_from_json(&name, image, env, env_from, 0);
            let (deployment_guard, deployment) =
                ResourceGuard::create(deployment_api.clone(), &deployment, delete_after_fail)
                    .await
                    .unwrap();
            guards.push(deployment_guard);
            let rollout = argo_rollout_from_json(&name, SpecSource::WorkloadRef(&deployment));
            create_rollout(
                rollout_api,
                &rollout,
                delete_after_fail,
                &mut guards,
                &name,
                namespace,
                &kube_client,
            )
            .await;
        }
        TestWorkloadType::ArgoRolloutWithTemplate => {
            let rollout = argo_rollout_from_json(
                &name,
                SpecSource::PodTemplate(get_pod_template_json_value(&name, image, env, env_from)),
            );
            create_rollout(
                rollout_api,
                &rollout,
                delete_after_fail,
                &mut guards,
                &name,
                namespace,
                &kube_client,
            )
            .await;
        }
    }

    // `Service`
    let mut service = service_from_json(&name, service_type);
    if ipv6_only {
        set_ipv6_only(&mut service);
    }
    let (service_guard, service) =
        ResourceGuard::create(service_api.clone(), &service, delete_after_fail)
            .await
            .unwrap();
    guards.push(service_guard);

    let ready_pod = watch::wait_until_pods_ready(&service, 1, kube_client.clone())
        .await
        .into_iter()
        .next()
        .unwrap();
    let pod_name = ready_pod.metadata.name.unwrap();

    println!(
        "{} done creating service {name} in namespace {namespace}",
        format_time(),
    );

    KubeService {
        name,
        namespace: namespace.to_string(),
        pod_name,
        guards,
        workload_type,
    }
}

#[cfg(not(feature = "operator"))]
#[fixture]
pub async fn service_for_mirrord_ls(
    #[default("default")] namespace: &str,
    #[default("NodePort")] service_type: &str,
    #[default("ghcr.io/metalbear-co/mirrord-pytest:latest")] image: &str,
    #[default("http-echo")] service_name: &str,
    #[default(true)] randomize_name: bool,
    #[future] kube_client: Client,
) -> KubeService {
    basic_service(
        namespace,
        service_type,
        image,
        service_name,
        randomize_name,
        kube_client,
    )
    .await
}

/// Service that should only be reachable from inside the cluster, as a communication partner
/// for testing outgoing traffic. If this service receives the application's messages, they
/// must have been intercepted and forwarded via the agent to be sent from the impersonated pod.
#[fixture]
pub async fn udp_logger_service(#[future] kube_client: Client) -> KubeService {
    basic_service(
        "default",
        "ClusterIP",
        "ghcr.io/metalbear-co/mirrord-node-udp-logger:latest",
        "udp-logger",
        true,
        kube_client,
    )
    .await
}

/// Service that listens on port 80 and returns `remote: <DATA>` when getting `<DATA>` directly
/// over TCP, not HTTP.
#[fixture]
pub async fn tcp_echo_service(#[future] kube_client: Client) -> KubeService {
    basic_service(
        "default",
        "NodePort",
        "ghcr.io/metalbear-co/mirrord-tcp-echo:latest",
        "tcp-echo",
        true,
        kube_client,
    )
    .await
}

/// [Service](https://github.com/metalbear-co/test-images/blob/main/websocket/app.mjs)
/// that listens on port 80 and returns `remote: <DATA>` when getting `<DATA>` over a websocket
/// connection, allowing us to test HTTP upgrade requests.
#[fixture]
pub async fn websocket_service(#[future] kube_client: Client) -> KubeService {
    basic_service(
        "default",
        "NodePort",
        "ghcr.io/metalbear-co/mirrord-websocket:latest",
        "websocket",
        true,
        kube_client,
    )
    .await
}

#[fixture]
pub async fn http2_service(#[future] kube_client: Client) -> KubeService {
    basic_service(
        "default",
        "NodePort",
        "ghcr.io/metalbear-co/mirrord-pytest:latest",
        "http2-echo",
        true,
        kube_client,
    )
    .await
}

/// Service that listens on port 80 and returns `remote: <DATA>` when getting `<DATA>` directly
/// over TCP, not HTTP.
#[fixture]
pub async fn hostname_service(#[future] kube_client: Client) -> KubeService {
    basic_service(
        "default",
        "NodePort",
        "ghcr.io/metalbear-co/mirrord-pytest:latest",
        "hostname-echo",
        true,
        kube_client,
    )
    .await
}

#[fixture]
pub async fn random_namespace_self_deleting_service(#[future] kube_client: Client) -> KubeService {
    let namespace = format!("random-namespace-{}", random_string());
    basic_service(
        &namespace,
        "NodePort",
        "ghcr.io/metalbear-co/mirrord-pytest:latest",
        "pytest-echo",
        true,
        kube_client,
    )
    .await
}

#[fixture]
pub async fn go_statfs_service(#[future] kube_client: Client) -> KubeService {
    basic_service(
        "default",
        "ClusterIP",
        "ghcr.io/metalbear-co/mirrord-go-statfs:latest",
        "go-statfs",
        true,
        kube_client,
    )
    .await
}

#[fixture]
pub async fn fs_service(#[future] kube_client: kube::Client) -> KubeService {
    let namespace = format!("e2e-tests-fs-policies-{}", crate::utils::random_string());

    basic_service(
        &namespace,
        "NodePort",
        "ghcr.io/metalbear-co/mirrord-pytest:latest",
        "fs-policy-e2e-test-service",
        false,
        kube_client,
    )
    .await
}

#[fixture]
pub async fn rollout_service(
    #[default("default")] namespace: &str,
    #[default("NodePort")] service_type: &str,
    #[default("ghcr.io/metalbear-co/mirrord-pytest:latest")] image: &str,
    #[default("http-echo")] service_name: &str,
    #[default(true)] randomize_name: bool,
    #[future] kube_client: Client,
) -> KubeService {
    internal_service(
        namespace,
        service_type,
        image,
        service_name,
        randomize_name,
        kube_client.await,
        default_env(),
        None,
        None,
        false,
        TestWorkloadType::ArgoRolloutWithWorkloadRef,
    )
    .await
}
