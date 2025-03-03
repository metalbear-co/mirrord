use std::net::{Ipv4Addr, SocketAddr};

use k8s_openapi::api::core::v1::{Pod, Service};
use kube::{api::ListParams, Api, Client};
use tokio::{
    net::TcpListener,
    task::{JoinHandle, JoinSet},
};

pub struct PortForwarder {
    address: SocketAddr,
    handle: JoinHandle<()>,
}

impl PortForwarder {
    pub async fn new(client: Client, pod_name: &str, pod_namespace: &str, port: u16) -> Self {
        let api = Api::namespaced(client, pod_namespace);

        let _ = api.portforward(pod_name, &[port]).await.unwrap();

        let listener = TcpListener::bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();

        let handle = tokio::spawn(Self::background_task(
            listener,
            api,
            pod_name.to_string(),
            port,
        ));

        Self { address, handle }
    }

    pub async fn new_for_service(client: Client, service: &Service, port: u16) -> Self {
        let selector = service
            .spec
            .as_ref()
            .unwrap()
            .selector
            .as_ref()
            .unwrap()
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(",");

        let pod = Api::<Pod>::namespaced(
            client.clone(),
            service.metadata.namespace.as_deref().unwrap(),
        )
        .list(&ListParams {
            label_selector: Some(selector),
            ..Default::default()
        })
        .await
        .unwrap()
        .items
        .into_iter()
        .next()
        .unwrap();

        Self::new(
            client,
            pod.metadata.name.as_deref().unwrap(),
            pod.metadata.namespace.as_deref().unwrap(),
            port,
        )
        .await
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    async fn background_task(listener: TcpListener, api: Api<Pod>, pod_name: String, port: u16) {
        let mut tasks = JoinSet::new();

        loop {
            let (mut local_conn, _) = listener.accept().await.unwrap();
            let api = api.clone();
            let pod_name = pod_name.clone();

            tasks.spawn(async move {
                let mut portforwarder = api.portforward(&pod_name, &[port]).await.unwrap();
                let mut pod_conn = portforwarder.take_stream(port).unwrap();
                tokio::io::copy_bidirectional(&mut local_conn, &mut pod_conn)
                    .await
                    .unwrap();
                std::mem::drop(pod_conn);
                std::mem::drop(local_conn);
                portforwarder.abort();
            });
        }
    }
}

impl Drop for PortForwarder {
    fn drop(&mut self) {
        self.handle.abort();
    }
}
