use envconfig::Envconfig;
use futures::{StreamExt, TryStreamExt};
use k8s_openapi::api::{batch::v1::Job, core::v1::Pod};
use kube::{
    api::{Api, ListParams, Portforwarder, PostParams},
    core::WatchEvent,
    runtime::wait::{await_condition, conditions::is_pod_running},
    Client, Config,
};
use rand::distributions::{Alphanumeric, DistString};
use serde_json::json;
use tracing::{error, warn};

use crate::config::LayerConfig;

struct RuntimeData {
    container_id: String,
    container_runtime: String,
    node_name: String,
    socket_path: String,
}

impl RuntimeData {
    async fn from_k8s(client: Client, pod_name: &str, pod_namespace: &str) -> Self {
        let pods_api: Api<Pod> = Api::namespaced(client, pod_namespace);
        let pod = pods_api.get(pod_name).await.unwrap();
        let node_name = &pod.spec.unwrap().node_name;
        let container_statuses = pod.status.unwrap().container_statuses.unwrap();
        let container_info = container_statuses
            .first()
            .unwrap()
            .container_id
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

pub async fn create_agent(config: LayerConfig) -> Portforwarder {
    let LayerConfig {
        agent_rust_log,
        agent_namespace,
        agent_image,
        impersonated_pod_name,
        impersonated_pod_namespace,
        ..
    } = config;

    let env_config = LayerConfig::init_from_env().unwrap();

    let client = if env_config.accept_invalid_certificates {
        let mut config = Config::infer().await.unwrap();
        config.accept_invalid_certs = true;
        warn!("Accepting invalid certificates");
        Client::try_from(config).unwrap()
    } else {
        Client::try_default().await.unwrap()
    };

    let runtime_data = RuntimeData::from_k8s(
        client.clone(),
        &impersonated_pod_name,
        &impersonated_pod_namespace,
    )
    .await;

    let agent_job_name = format!(
        "mirrord-agent-{}",
        Alphanumeric
            .sample_string(&mut rand::thread_rng(), 10)
            .to_lowercase()
    );

    let agent_image = agent_image.unwrap_or_else(|| {
        concat!("ghcr.io/metalbear-co/mirrord:", env!("CARGO_PKG_VERSION")).to_string()
    });

    let agent_pod: Job =
        serde_json::from_value(json!({ // Only Jobs support self deletion after completion
                "metadata": {
                    "name": agent_job_name
                },
                "spec": {
                "ttlSecondsAfterFinished": env_config.agent_ttl,

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
                            "imagePullPolicy": "Always",
                            // "imagePullPolicy": "IfNotPresent",
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
                            ],
                            "env": [{"name": "RUST_LOG", "value": agent_rust_log}],
                        }
                    ]
                }
            }
        }
            }
        ))
        .unwrap();

    let jobs_api: Api<Job> = Api::namespaced(client.clone(), &agent_namespace);
    jobs_api
        .create(&PostParams::default(), &agent_pod)
        .await
        .unwrap();

    let pods_api: Api<Pod> = Api::namespaced(client.clone(), &agent_namespace);
    let params = ListParams::default()
        .labels(&format!("job-name={}", agent_job_name))
        .timeout(10);

    let mut stream = pods_api.watch(&params, "0").await.unwrap().boxed();
    while let Some(status) = stream.try_next().await.unwrap() {
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
        .list(&ListParams::default().labels(&format!("job-name={}", agent_job_name)))
        .await
        .unwrap();
    let pod = pods.items.first().unwrap();
    let pod_name = pod.metadata.name.clone().unwrap();
    let running = await_condition(pods_api.clone(), &pod_name, is_pod_running());

    let _ = tokio::time::timeout(std::time::Duration::from_secs(20), running)
        .await
        .unwrap();
    pods_api.portforward(&pod_name, &[61337]).await.unwrap()
}
