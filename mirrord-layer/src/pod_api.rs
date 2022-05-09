use k8s_openapi::api::core::v1::Pod;
use kube::{
    api::{Api, Portforwarder, PostParams},
    runtime::wait::{await_condition, conditions::is_pod_running},
    Client,
};
use rand::distributions::{Alphanumeric, DistString};
use serde_json::json;

struct RuntimeData {
    container_id: String,
    node_name: String,
}

impl RuntimeData {
    async fn from_k8s(client: Client, pod_name: &str, pod_namespace: &str) -> Self {
        let pods_api: Api<Pod> = Api::namespaced(client, pod_namespace);
        let pod = pods_api.get(pod_name).await.unwrap();
        let node_name = &pod.spec.unwrap().node_name;
        let container_statuses = pod.status.unwrap().container_statuses.unwrap();
        let container_id = container_statuses
            .first()
            .unwrap()
            .container_id
            .as_ref()
            .unwrap()
            .split("//")
            .last()
            .unwrap();
        RuntimeData {
            container_id: container_id.to_string(),
            node_name: node_name.as_ref().unwrap().to_string(),
        }
    }
}

pub async fn create_agent(
    pod_name: &str,
    pod_namespace: &str,
    agent_namespace: &str,
    log_level: String,
) -> Portforwarder {
    let client = Client::try_default().await.unwrap();
    let runtime_data = RuntimeData::from_k8s(client.clone(), pod_name, pod_namespace).await;
    let agent_pod_name = format!(
        "mirrord-agent-{}",
        Alphanumeric
            .sample_string(&mut rand::thread_rng(), 10)
            .to_lowercase()
    );

    let agent_pod: Pod = serde_json::from_value(json!({
        "metadata": {
            "name": agent_pod_name
        },
        "spec": {
            "hostPID": true,
            "nodeName": runtime_data.node_name,
            "restartPolicy": "Never",
            "volumes": [
                {
                    "name": "containerd",
                    "hostPath": {
                        "path": "/run/containerd/containerd.sock"
                    }
                }
            ],
            "containers": [
                {
                    "name": "mirrord-agent",
                    "image": "ghcr.io/metalbear-co/mirrord-agent:2.0.0-alpha-3",
                    "imagePullPolicy": "Always",
                    "securityContext": {
                        "privileged": true
                    },
                    "volumeMounts": [
                        {
                            "mountPath": "/run/containerd/containerd.sock",
                            "name": "containerd"
                        }
                    ],
                    "command": [
                        "./mirrord-agent",
                        "--container-id",
                        runtime_data.container_id,
                        "-t",
                        "60"
                    ],
                    "env": [{"name": "RUST_LOG", "value": log_level}],
                }
            ]
        }
    }))
    .unwrap();

    let pods_api: Api<Pod> = Api::namespaced(client, agent_namespace);
    pods_api
        .create(&PostParams::default(), &agent_pod)
        .await
        .unwrap();

    //   Wait until the pod is running, otherwise we get 500 error.
    let running = await_condition(pods_api.clone(), &agent_pod_name, is_pod_running());
    let _ = tokio::time::timeout(std::time::Duration::from_secs(15), running)
        .await
        .unwrap();
    let pf = pods_api
        .portforward(&agent_pod_name, &[61337])
        .await
        .unwrap();

    pf
}
