//! Tests that spawn an agent and talk `mirrord-protocol` to it directly, with no CLI or layer.
//!
//! The layer only sends requests that a local process could make, but nothing stops a client from
//! sending the agent anything at all, so the agent's guarantees have to hold for requests no
//! layer would send. These tests send such requests, the way a broken or hostile client would.
#![cfg(test)]

use std::{path::PathBuf, time::Duration};

use k8s_openapi::api::core::v1::Pod;
use kube::Api;
use mirrord_config::{config::MirrordConfig, LayerConfig, LayerFileConfig};
use mirrord_kube::api::{
    container::ContainerConfig,
    kubernetes::{CreatedAgent, KubernetesAPI},
};
use mirrord_progress::NullProgress;
use mirrord_protocol::{
    file::{OpenFileRequest, OpenOptionsInternal, OpenRelativeFileRequest, ReadFileRequest},
    outgoing::{
        tcp::{DaemonTcpOutgoing, LayerTcpOutgoing},
        LayerConnect, SocketAddress, UnixAddr,
    },
    ClientMessage, DaemonMessage, FileRequest, FileResponse, RemoteResult,
};
use mirrord_protocol_io::{Client, Connection};
use rstest::rstest;
use serde_json::json;

use crate::utils::{
    client::kube_client, random_string, resource_guard::ResourceGuard, watch::wait_until_pod_ready,
    KubeClient, CONTAINER_NAME, PRESERVE_FAILED_ENV_NAME, TEST_RESOURCE_LABEL,
};

/// A path that exists in the agent's filesystem and never in the target's: the agent's binary.
const AGENT_ONLY_PATH: &str = "/mirrord-agent";

/// The node's container runtime socket, in the agent's filesystem: the job agent mounts the
/// node's `/run` at `/host/run`, and a targeted agent sees the node's root at `/proc/1/root`.
const NODE_SOCKETS: [&str; 2] = [
    "/host/run/containerd/containerd.sock",
    "/proc/1/root/run/containerd/containerd.sock",
];

/// What the target pod creates before it reports ready.
///
/// The target runs as a user other than root, so its symlinks in `/tmp`, a sticky
/// world-writable directory of root, are the kind that `fs.protected_symlinks` stops root from
/// following. A projected service account token is such a symlink (#5357).
const TARGET_SETUP: &str = r#"
echo -n target-data > /tmp/data
ln -s data /tmp/link
ln -s /mirrord-agent /tmp/absolute-escape
ln -s ../../../../../../mirrord-agent /tmp/relative-escape
exec python3 -c '
import socket, time
s = socket.socket(socket.AF_UNIX)
s.bind("/tmp/app.sock")
s.listen()
time.sleep(10**9)
'
"#;

/// A pod that runs [`TARGET_SETUP`] as uid 1000, and the guard that deletes it.
async fn target_pod(kube_client: &KubeClient) -> (String, ResourceGuard) {
    let name = format!("agent-protocol-{}", random_string());
    let pod = serde_json::from_value::<Pod>(json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": name,
            "labels": { TEST_RESOURCE_LABEL.0: TEST_RESOURCE_LABEL.1 },
        },
        "spec": {
            "containers": [{
                "name": CONTAINER_NAME,
                "image": crate::utils::images::PYTEST_IMAGE,
                "imagePullPolicy": "IfNotPresent",
                "command": ["sh", "-c", TARGET_SETUP],
                // On the container, since one on the pod would apply to an ephemeral agent too.
                "securityContext": { "runAsUser": 1000 },
                "readinessProbe": {
                    "exec": { "command": ["test", "-S", "/tmp/app.sock"] },
                    "periodSeconds": 1,
                },
            }],
        },
    }))
    .unwrap();

    let (guard, _) = ResourceGuard::create(
        Api::namespaced(kube_client.get_client(), "default"),
        &pod,
        std::env::var_os(PRESERVE_FAILED_ENV_NAME).is_none(),
        kube_client.get_config(),
    )
    .await
    .unwrap();
    wait_until_pod_ready(&name, "default", kube_client.get_client()).await;

    (name, guard)
}

/// A connection to an agent that targets a pod, the way the CLI makes one.
struct AgentClient(Connection<Client>);

impl AgentClient {
    async fn spawn(pod: &str) -> Self {
        let config: LayerConfig = serde_json::from_value::<LayerFileConfig>(json!({
            "target": {
                "path": format!("pod/{pod}/container/{CONTAINER_NAME}"),
                "namespace": "default",
            },
            "agent": { "ephemeral": cfg!(feature = "ephemeral") },
        }))
        .unwrap()
        .generate_config(&mut Default::default())
        .unwrap();

        let api = KubernetesAPI::create(&config, &NullProgress).await.unwrap();
        let CreatedAgent { connect_info, .. } = api
            .create_agent(
                &mut NullProgress,
                &config.target,
                None,
                ContainerConfig::default(),
            )
            .await
            .unwrap();
        let stream = api
            .create_connection_portforward(connect_info)
            .await
            .unwrap();

        let mut agent = Self(Connection::from_stream(stream));
        let response = agent
            .request(ClientMessage::SwitchProtocolVersion(
                mirrord_protocol::VERSION.clone(),
            ))
            .await;
        assert!(
            matches!(response, DaemonMessage::SwitchProtocolVersionResponse(..)),
            "{response:?}"
        );

        agent
    }

    async fn request(&mut self, message: ClientMessage) -> DaemonMessage {
        self.0.send(message).await;
        tokio::time::timeout(Duration::from_secs(30), self.0.recv())
            .await
            .expect("the agent didn't respond in time")
            .expect("the agent closed the connection")
    }

    /// Opens `path` read-only, relative to the remote descriptor `relative_fd` when given one.
    async fn open(&mut self, relative_fd: Option<u64>, path: &str) -> RemoteResult<u64> {
        let path = PathBuf::from(path);
        let open_options = OpenOptionsInternal {
            read: true,
            ..Default::default()
        };
        let request = match relative_fd {
            Some(relative_fd) => FileRequest::OpenRelative(OpenRelativeFileRequest {
                relative_fd,
                path,
                open_options,
            }),
            None => FileRequest::Open(OpenFileRequest { path, open_options }),
        };

        match self.request(ClientMessage::FileRequest(request)).await {
            DaemonMessage::File(FileResponse::Open(result)) => result.map(|response| response.fd),
            response => panic!("unexpected open response: {response:?}"),
        }
    }

    async fn read(&mut self, remote_fd: u64) -> Vec<u8> {
        let request = FileRequest::Read(ReadFileRequest {
            remote_fd,
            buffer_size: 4096,
        });

        match self.request(ClientMessage::FileRequest(request)).await {
            DaemonMessage::File(FileResponse::Read(result)) => result.unwrap().bytes.to_vec(),
            response => panic!("unexpected read response: {response:?}"),
        }
    }

    async fn connect_unix(&mut self, path: &str) -> RemoteResult<()> {
        let remote_address = SocketAddress::Unix(UnixAddr::Pathname(path.into()));
        let request = LayerTcpOutgoing::Connect(LayerConnect { remote_address });

        match self.request(ClientMessage::TcpOutgoing(request)).await {
            DaemonMessage::TcpOutgoing(DaemonTcpOutgoing::Connect(result)) => result.map(drop),
            response => panic!("unexpected connect response: {response:?}"),
        }
    }
}

/// The agent opens a file behind a symlink that `fs.protected_symlinks` stops it from
/// following, as the target itself can (#5357).
///
/// Reproduces only on a node with `fs.protected_symlinks` set, which is the default on most
/// distributions.
#[cfg_attr(not(any(feature = "ephemeral", feature = "job")), ignore)]
#[rstest]
#[tokio::test]
async fn opens_through_protected_symlink(#[future] kube_client: KubeClient) {
    let kube_client = kube_client.await;
    let (pod, _guard) = target_pod(&kube_client).await;
    let mut agent = AgentClient::spawn(&pod).await;

    let fd = agent.open(None, "/tmp/link").await.unwrap();

    assert_eq!(agent.read(fd).await, b"target-data");
}

/// No path in a request reaches past the target's root, however the client builds it.
#[cfg_attr(not(any(feature = "ephemeral", feature = "job")), ignore)]
#[rstest]
#[tokio::test]
async fn paths_stay_in_target_root(#[future] kube_client: KubeClient) {
    let kube_client = kube_client.await;
    let (pod, _guard) = target_pod(&kube_client).await;
    let mut agent = AgentClient::spawn(&pod).await;

    let root = agent.open(None, "/").await.unwrap();
    let tmp = agent.open(None, "/tmp").await.unwrap();
    let data = agent.open(Some(tmp), "data").await.unwrap();
    assert_eq!(agent.read(data).await, b"target-data");
    agent.connect_unix("/tmp/app.sock").await.unwrap();

    let escapes = [
        (None, AGENT_ONLY_PATH.to_owned()),
        (None, format!("/../../..{AGENT_ONLY_PATH}")),
        (None, format!("/proc/1/root{AGENT_ONLY_PATH}")),
        (None, "/tmp/absolute-escape".to_owned()),
        (None, "/tmp/relative-escape".to_owned()),
        (Some(root), AGENT_ONLY_PATH.to_owned()),
        (Some(tmp), AGENT_ONLY_PATH.to_owned()),
        (Some(tmp), format!("../../..{AGENT_ONLY_PATH}")),
        (Some(tmp), "absolute-escape".to_owned()),
    ];
    let mut escaped = Vec::new();
    for (relative_fd, path) in escapes {
        if agent.open(relative_fd, &path).await.is_ok() {
            escaped.push(format!("open {path:?} from {relative_fd:?}"));
        }
    }
    for path in NODE_SOCKETS {
        if agent.connect_unix(path).await.is_ok() {
            escaped.push(format!("connect {path:?}"));
        }
    }

    assert!(escaped.is_empty(), "{escaped:#?}");
}
