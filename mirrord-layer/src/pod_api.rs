use anyhow::{Context, Result};
use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::{
    batch::v1::Job,
    core::v1::{EphemeralContainer, Pod},
};
use kube::{
    api::{Api, ListParams, Portforwarder, PostParams},
    core::WatchEvent,
    runtime::wait::{await_condition, conditions::is_pod_running},
    Client, Config,
};
use rand::distributions::{Alphanumeric, DistString};
use serde_json::{json, to_vec};
use tracing::{debug, error, info, warn};

use crate::{config::LayerConfig, error::LayerError};

struct RuntimeData {
    container_id: String,
    container_runtime: String,
    node_name: String,
    socket_path: String,
}

impl RuntimeData {
    async fn from_k8s(
        client: Client,
        pod_name: &str,
        pod_namespace: &str,
        container_name: &Option<String>,
    ) -> Self {
        let pods_api: Api<Pod> = Api::namespaced(client, pod_namespace);
        let pod = pods_api.get(pod_name).await.unwrap();
        let node_name = &pod.spec.unwrap().node_name;
        let container_statuses = &pod.status.unwrap().container_statuses.unwrap();
        let container_info = if let Some(container_name) = container_name {
            &container_statuses
                .iter()
                .find(|&status| &status.name == container_name)
                .with_context(|| {
                    format!(
                        "no container named {} found in namespace={}, pod={}",
                        &container_name, &pod_namespace, &pod_name
                    )
                })
                .unwrap()
                .container_id
        } else {
            info!("No container name specified, defaulting to first container found");
            &container_statuses.first().unwrap().container_id
        };

        let container_info = container_info
            .as_ref()
            .unwrap()
            .split("://")
            .collect::<Vec<&str>>();

        let (container_runtime, socket_path) = match container_info.first() {
            Some(&"docker") => ("docker", "/var/run/docker.sock"),
            Some(&"containerd") => ("containerd", "/run/containerd/containerd.sock"),
            _ => panic!("unsupported container runtime"),
        };

        let container_id = container_info.last().unwrap();

        RuntimeData {
            container_id: container_id.to_string(),
            container_runtime: container_runtime.to_string(),
            node_name: node_name.as_ref().unwrap().to_string(),
            socket_path: socket_path.to_string(),
        }
    }
}

pub(crate) async fn create_agent(
    config: LayerConfig,
    connection_port: u16,
) -> Result<Portforwarder, LayerError> {
    let LayerConfig {
        agent_image,
        agent_namespace,
        impersonated_pod_name,
        impersonated_pod_namespace,
        impersonated_container_name,
        ephemeral_container,
        accept_invalid_certificates,
        ..
    } = config.clone();

    let client = if accept_invalid_certificates {
        let mut config = Config::infer().await?;
        config.accept_invalid_certs = true;
        warn!("Accepting invalid certificates");
        Client::try_from(config).map_err(LayerError::KubeError)?
    } else {
        Client::try_default().await.map_err(LayerError::KubeError)?
    };

    let pods_api: Api<Pod> = Api::namespaced(client.clone(), &agent_namespace);

    let agent_image = agent_image.unwrap_or_else(|| {
        concat!("ghcr.io/metalbear-co/mirrord:", env!("CARGO_PKG_VERSION")).to_string()
    });

    let pod_name = if ephemeral_container {
        create_ephemeral_container_agent(&config, agent_image, &pods_api, connection_port).await?
    } else {
        let runtime_data = RuntimeData::from_k8s(
            client.clone(),
            &impersonated_pod_name,
            &impersonated_pod_namespace,
            &impersonated_container_name,
        )
        .await;
        let jobs_api: Api<Job> = Api::namespaced(client.clone(), &agent_namespace);

        create_job_pod_agent(
            &config,
            agent_image,
            &pods_api,
            runtime_data,
            &jobs_api,
            connection_port,
        )
        .await?
    };
    pods_api
        .portforward(&pod_name, &[connection_port])
        .await
        .map_err(LayerError::KubeError)
}

fn get_agent_name() -> String {
    let agent_name = format!(
        "mirrord-agent-{}",
        Alphanumeric
            .sample_string(&mut rand::thread_rng(), 10)
            .to_lowercase()
    );
    agent_name
}

async fn create_ephemeral_container_agent(
    config: &LayerConfig,
    agent_image: String,
    pods_api: &Api<Pod>,
    connection_port: u16,
) -> Result<String, LayerError> {
    warn!("Ephemeral Containers is an experimental feature
              >> Refer https://kubernetes.io/docs/concepts/workloads/pods/ephemeral-containers/ for more info");

    let mirrord_agent_name = get_agent_name();

    let ephemeral_container: EphemeralContainer = serde_json::from_value(json!({
        "name": mirrord_agent_name,
        "image": agent_image,
        "imagePullPolicy": config.image_pull_policy,
        "targetContainerName": config.impersonated_container_name,
        "env": [{"name": "RUST_LOG", "value": config.agent_rust_log}],
        "command": [
            "./mirrord-agent",
            "-t",
            "30",
            "-l",
            connection_port.to_string(),
            "-e",
        ],
    }))?;
    debug!("Requesting ephemeral_containers_subresource");

    let mut ephemeral_containers_subresource = pods_api
        .get_subresource("ephemeralcontainers", &config.impersonated_pod_name)
        .await
        .map_err(LayerError::KubeError)?;

    let mut spec = ephemeral_containers_subresource
        .spec
        .as_mut()
        .ok_or_else(|| LayerError::PodSpecNotFound(config.impersonated_pod_name.clone()))?;

    spec.ephemeral_containers = match spec.ephemeral_containers.clone() {
        Some(mut ephemeral_containers) => {
            ephemeral_containers.push(ephemeral_container);
            Some(ephemeral_containers)
        }
        None => Some(vec![ephemeral_container]),
    };

    pods_api
        .replace_subresource(
            "ephemeralcontainers",
            &config.impersonated_pod_name,
            &PostParams::default(),
            to_vec(&ephemeral_containers_subresource).unwrap(),
        )
        .await
        .map_err(LayerError::KubeError)?;

    let params = ListParams::default()
        .fields(&format!("metadata.name={}", "mirrord_agent"))
        .timeout(60);

    let mut stream = pods_api
        .watch(&params, "0")
        .await
        .map_err(LayerError::KubeError)?
        .boxed();

    debug!("waiting for container to be ready");
    while let Some(status) = stream.try_next().await? {
        match status {
            WatchEvent::Added(_) => break,
            WatchEvent::Error(s) => {
                error!("Error watching pod: {:?}", s);
                break;
            }
            _ => {}
        }
    }
    debug!("container is ready");
    Ok(config.impersonated_pod_name.clone())
}

async fn create_job_pod_agent(
    config: &LayerConfig,
    agent_image: String,
    pods_api: &Api<Pod>,
    runtime_data: RuntimeData,
    job_api: &Api<Job>,
    connection_port: u16,
) -> Result<String, LayerError> {
    let mirrord_agent_job_name = get_agent_name();

    let agent_pod: Job =
        serde_json::from_value(json!({ // Only Jobs support self deletion after completion
                "metadata": {
                    "name": mirrord_agent_job_name,
                    "labels": {
                        "app": "mirrord"
                    }
                },
                "spec": {
                "ttlSecondsAfterFinished": config.agent_ttl,

                    "template": {
                "spec": {
                    "hostPID": true,
                    "nodeName": runtime_data.node_name,
                    "restartPolicy": "Never",
                    "volumes": [
                        {
                            "name": "sockpath",
                            "hostPath": {
                                "path": runtime_data.socket_path
                            }
                        }
                    ],
                    "containers": [
                        {
                            "name": "mirrord-agent",
                            "image": agent_image,
                            "imagePullPolicy": config.image_pull_policy,
                            "securityContext": {
                                "privileged": true,
                            },
                            "volumeMounts": [
                                {
                                    "mountPath": runtime_data.socket_path,
                                    "name": "sockpath"
                                }
                            ],
                            "command": [
                                "./mirrord-agent",
                                "--container-id",
                                runtime_data.container_id,
                                "--container-runtime",
                                runtime_data.container_runtime,
                                "-t",
                                "30",
                                "-l",
                                connection_port.to_string(),
                            ],
                            "env": [{"name": "RUST_LOG", "value": config.agent_rust_log}],
                        }
                    ]
                }
            }
        }
            }
        ))?;
    job_api
        .create(&PostParams::default(), &agent_pod)
        .await
        .map_err(LayerError::KubeError)?;

    let params = ListParams::default()
        .labels(&format!("job-name={}", mirrord_agent_job_name))
        .timeout(60);

    let mut stream = pods_api
        .watch(&params, "0")
        .await
        .map_err(LayerError::KubeError)?
        .boxed();

    while let Some(status) = stream.try_next().await? {
        match status {
            WatchEvent::Added(_) => break,
            WatchEvent::Error(s) => {
                error!("Error watching pods: {:?}", s);
                break;
            }
            _ => {}
        }
    }

    let pods = pods_api
        .list(&ListParams::default().labels(&format!("job-name={}", mirrord_agent_job_name)))
        .await
        .map_err(LayerError::KubeError)?;

    let pod = pods.items.first().unwrap();
    let pod_name = pod.metadata.name.clone().unwrap();
    let running = await_condition(pods_api.clone(), &pod_name, is_pod_running());

    let _ = tokio::time::timeout(std::time::Duration::from_secs(60), running)
        .await
        .map_err(|_| LayerError::TimeOutError)?; // TODO: convert the elapsed error to string?
    Ok(pod_name)
}
