use k8s_openapi::api::core::v1::Pod;
use kube::{
    api::{Api, Portforwarder, PostParams},
    runtime::wait::{await_condition, conditions::is_pod_running},
    Client,
};
use rand::distributions::{Alphanumeric, DistString};
use serde_json::json;
#[path = "config.rs"]
mod config;
use config::CONFIG;

pub async fn create_agent() -> Portforwarder {
    // Create Agent
    let client = Client::try_default().await.unwrap();
    let pods: Api<Pod> = Api::namespaced(client, "default");
    let pod = pods.get(&CONFIG.impersonated_pod_name).await.unwrap();
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
            "nodeName": node_name,
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
                        container_id,
                        "-t",
                        "60"
                    ],
                    "env": [{"name": "RUST_LOG", "value": CONFIG.agent_rust_log}],
                }
            ]
        }
    }))
    .unwrap();
    pods.create(&PostParams::default(), &agent_pod)
        .await
        .unwrap();

    //   Wait until the pod is running, otherwise we get 500 error.
    let running = await_condition(pods.clone(), &agent_pod_name, is_pod_running());
    let _ = tokio::time::timeout(std::time::Duration::from_secs(15), running)
        .await
        .unwrap();
    let pf = pods.portforward(&agent_pod_name, &[61337]).await.unwrap();

    pf
}
