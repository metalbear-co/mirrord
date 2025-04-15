#![cfg(feature = "operator")]
use cluster_resource::{operator::*, *};
use k8s_openapi::api::{
    apps::v1::Deployment,
    core::v1::{Namespace, Service},
};
use kube::{api::DeleteParams, Api, Client};
use kube_service::KubeService;
use resource_guard::ResourceGuard;
use rstest::*;
use serde_json::json;

use super::{cluster_resource, kube_service, resource_guard};
use crate::utils::{
    default_env, format_time, kube_client, random_string, watch, PRESERVE_FAILED_ENV_NAME,
    TEST_RESOURCE_LABEL,
};

#[fixture]
pub async fn service_for_mirrord_ls(
    #[default("default")] namespace: &str,
    #[default("NodePort")] service_type: &str,
    #[default("ghcr.io/metalbear-co/mirrord-pytest:latest")] image: &str,
    #[default("http-echo")] service_name: &str,
    #[default(true)] randomize_name: bool,
    #[future] kube_client: Client,
) -> KubeService {
    use k8s_openapi::api::{
        apps::v1::StatefulSet,
        batch::v1::{CronJob, Job},
    };

    let delete_after_fail = std::env::var_os(PRESERVE_FAILED_ENV_NAME).is_none();

    let kube_client = kube_client.await;
    let namespace_api: Api<Namespace> = Api::all(kube_client.clone());
    let deployment_api: Api<Deployment> = Api::namespaced(kube_client.clone(), namespace);
    let stateful_set_api: Api<StatefulSet> = Api::namespaced(kube_client.clone(), namespace);
    let cron_job_api: Api<CronJob> = Api::namespaced(kube_client.clone(), namespace);
    let job_api: Api<Job> = Api::namespaced(kube_client.clone(), namespace);
    let service_api: Api<Service> = Api::namespaced(kube_client.clone(), namespace);

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
        let _ = stateful_set_api.delete(service_name, &delete_params).await;
        let _ = cron_job_api.delete(service_name, &delete_params).await;
        let _ = job_api.delete(service_name, &delete_params).await;

        service_name.to_string()
    };

    println!(
        "{} creating service {name:?} in namespace {namespace:?}",
        format_time()
    );

    let namespace_resource: Namespace = serde_json::from_value(json!({
        "apiVersion": "v1",
        "kind": "Namespace",
        "metadata": {
            "name": namespace,
            "labels": {
                TEST_RESOURCE_LABEL.0: TEST_RESOURCE_LABEL.1,
            }
        },
    }))
    .unwrap();
    // Create namespace and wrap it in ResourceGuard if it does not yet exist.
    let namespace_guard = ResourceGuard::create(
        namespace_api.clone(),
        &namespace_resource,
        delete_after_fail,
    )
    .await
    .ok();

    // `Deployment`
    let deployment = deployment_from_json(&name, image, default_env());
    let (deployment_guard, deployment) =
        ResourceGuard::create(deployment_api.clone(), &deployment, delete_after_fail)
            .await
            .unwrap();

    // `Service`
    let service = service_from_json(&name, service_type);
    let (service_guard, service) =
        ResourceGuard::create(service_api.clone(), &service, delete_after_fail)
            .await
            .unwrap();

    // `StatefulSet`
    let stateful_set = stateful_set_from_json(&name, image);
    let (stateful_set_guard, _) =
        ResourceGuard::create(stateful_set_api.clone(), &stateful_set, delete_after_fail)
            .await
            .unwrap();

    // `CronJob`
    let cron_job = cron_job_from_json(&name, image);
    let (cron_job_guard, _) =
        ResourceGuard::create(cron_job_api.clone(), &cron_job, delete_after_fail)
            .await
            .unwrap();

    // `Job`
    let job = job_from_json(&name, image);
    let (job_guard, _) = ResourceGuard::create(job_api.clone(), &job, delete_after_fail)
        .await
        .unwrap();

    let pod_name = watch::wait_until_pods_ready(&service, 1, kube_client.clone())
        .await
        .into_iter()
        .next()
        .unwrap()
        .metadata
        .name
        .unwrap();

    println!(
        "{:?} done creating service {name} in namespace {namespace}",
        chrono::Utc::now()
    );

    KubeService {
        name,
        namespace: namespace.to_string(),
        service,
        deployment,
        guards: vec![
            deployment_guard,
            service_guard,
            stateful_set_guard,
            cron_job_guard,
            job_guard,
        ],
        namespace_guard: namespace_guard.map(|(guard, _)| guard),
        pod_name,
    }
}
