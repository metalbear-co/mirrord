#![cfg(test)]

use std::{
    fmt,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    ops::Not,
    process::Stdio,
};

use k8s_openapi::api::core::v1::{Pod, Service};
use kube::{api::ListParams, Api, Client};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, BufReader},
    process::{Child, Command},
};

/// Address of a deployed test [`Service`], accessible from test code.
///
/// This struct needs to be kept alive while the address is being used.
pub struct TestServiceAddr {
    /// The accessible address.
    pub addr: SocketAddr,
    /// Optional `minikube service ...` child process.
    ///
    /// This command sometimes needs to keep running in the background.
    proxy: Option<Child>,
    service_name: String,
    service_namespace: String,
}

impl TestServiceAddr {
    /// Fetches an accessible address for the given [`Service`].
    ///
    /// * If one of the [`Pod`]s has a public IP, that IP will be used;
    /// * Otherwise, if we're on Linux (**not** WSL) or `USE_MINIKUBE` env variable is set,
    ///   `minikube ip` and `minikube service --url ...` commands will be used;
    /// * Otherwise `127.0.0.1` will be used.
    pub async fn fetch(client: Client, service: &Service) -> Self {
        let result = Self::fetch_inner(client, service).await;

        println!("RESOLVED TEST SERVICE ADDRESS: {result:?}");

        result
    }

    /// Spawns a `minikube service ...` child process that handles portforwarding to the service.
    #[allow(unused)]
    async fn with_minikube_proxy(service: &Service) -> Self {
        let service_name = service.metadata.name.clone().unwrap();
        let service_namespace = service.metadata.namespace.clone().unwrap();

        let mut child = Command::new("minikube")
            .arg("service")
            .arg("--url")
            .arg("-n")
            .arg(&service_namespace)
            .arg("--format")
            .arg("{{.IP}}:{{.Port}}")
            .arg(&service_name)
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .spawn()
            .unwrap();

        let stdout = child.stdout.take().unwrap();
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();

        let addr = line.trim().parse::<SocketAddr>().inspect_err(|error| {
            println!(
                "invalid socket addr returned from `minikube service --url` ({}): {error}",
                line.trim()
            )
        });

        let addr = match addr {
            Ok(addr) => addr,
            Err(error) => {
                println!(
                    "invalid socket addr returned from `minikube service --url` ({}): {error}",
                    line.trim()
                );

                let mut stderr = child.stderr.take().unwrap();
                let mut buf = String::new();
                stderr.read_to_string(&mut buf).await.unwrap();

                let status = child.try_wait().unwrap();

                panic!(
                    "`minikube service --url` produed an invalid socket address, \
                    error=({error}) \
                    address=({}) \
                    stderr=({}) \
                    exit_status=({status:?})",
                    line.trim(),
                    buf,
                );
            }
        };

        child.stdout.replace(reader.into_inner());

        Self {
            addr,
            proxy: Some(child),
            service_name,
            service_namespace,
        }
    }

    fn is_ip_public(ip: &IpAddr) -> bool {
        match ip {
            IpAddr::V4(ip) => ip.is_private().not(),
            IpAddr::V6(ip) => {
                ip.is_unicast_link_local().not()
                    && ip.is_unique_local().not()
                    && ip.is_loopback().not()
            }
        }
    }

    /// Finds a service address that is accessible from outside of the cluster.
    ///
    /// If that fails, falls back to [`Self::with_minikube_proxy`].
    async fn fetch_inner(client: Client, service: &Service) -> Self {
        let service_name = service.metadata.name.clone().unwrap();
        let service_namespace = service.metadata.namespace.clone().unwrap();

        let pods = {
            let pod_api = Api::<Pod>::namespaced(client.clone(), &service_namespace);

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

            pod_api
                .list(&ListParams {
                    label_selector: Some(selector),
                    ..Default::default()
                })
                .await
                .unwrap()
                .items
        };

        let pod_ip = pods
            .into_iter()
            .filter_map(|pod| pod.status)
            .filter_map(|status| status.host_ip)
            .filter_map(|ip| {
                println!(
                    "FOUND HOST IP OF A SERVICE `{service_namespace}/{service_name}` POD: {ip}"
                );
                ip.parse::<IpAddr>().ok()
            })
            .find(Self::is_ip_public);

        let ip = match pod_ip {
            Some(ip) => ip,
            None => {
                if (cfg!(target_os = "linux") && !wsl::is_wsl())
                    || std::env::var("USE_MINIKUBE").is_ok()
                {
                    println!("NO HOST WITH PUBLIC IP FOUND, TRYING `minikube service --url`");
                    return Self::with_minikube_proxy(service).await;
                } else {
                    println!("ASSUMING NO MINIKUBE, USING LOCALHOST AS HOST IP");
                    // We assume it's either Docker for Mac or passed via wsl integration
                    Ipv4Addr::LOCALHOST.into()
                }
            }
        };

        let port = service
            .spec
            .iter()
            .flat_map(|service| &service.ports)
            .flatten()
            .find_map(|port| port.node_port)
            .unwrap();

        let addr = SocketAddr::new(ip, port.try_into().unwrap());

        Self {
            addr,
            proxy: None,
            service_name,
            service_namespace,
        }
    }
}

impl fmt::Debug for TestServiceAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TestServiceAddr")
            .field("addr", &self.addr)
            .field("uses_proxy_child", &self.proxy.is_some())
            .field("service_name", &self.service_name)
            .field("service_namespace", &self.service_namespace)
            .finish()
    }
}
