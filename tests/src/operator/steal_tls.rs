#![cfg(feature = "operator")]

use std::{
    collections::{BTreeMap, HashMap},
    net::SocketAddr,
    time::Duration,
};

use k8s_openapi::{
    api::{
        apps::v1::{Deployment, DeploymentSpec},
        core::v1::{
            Container, ContainerPort, EnvVar, Namespace, PodSpec, PodTemplateSpec, Probe, Secret,
            SecretVolumeSource, Service, ServicePort, ServiceSpec, TCPSocketAction, Volume,
            VolumeMount,
        },
    },
    apimachinery::pkg::{apis::meta::v1::LabelSelector, util::intstr::IntOrString},
};
use kube::{api::ObjectMeta, Api, Client};
use mirrord_agent_env::steal_tls::{
    AgentClientConfig, AgentServerConfig, StealPortTlsConfig, TlsAuthentication,
    TlsClientVerification, TlsServerVerification,
};
use mirrord_operator::crd::steal_tls::{MirrordTlsStealConfig, MirrordTlsStealConfigSpec};
use mirrord_tls_util::generate_cert;
use pem::{EncodeConfig, LineEnding, Pem};
use rcgen::CertifiedKey;
use rstest::rstest;

use crate::utils::{
    application::Application, kube_client, port_forwarder::PortForwarder,
    resource_guard::ResourceGuard, watch, PRESERVE_FAILED_ENV_NAME, TEST_RESOURCE_LABEL,
};

/// Test service deployed to the cluster.
///
/// Runs an HTTPS server from [this image](https://github.com/metalbear-co/test-images/tree/main/go-server).
struct GoServService {
    _guard: ResourceGuard,

    namespace: Namespace,
    deployment: Deployment,
    service: Service,
}

impl GoServService {
    const IMAGE_URL: &str = "ghcr.io/metalbear-co/mirrord-go-server";
    const REMOTE_SERVER_RESPONSE: &str = "HELLO FROM REMOTE";
    const SERVER_PORT_ENV_NAME: &str = "SERVER_PORT";
    const SERVER_MESSAGE_ENV_NAME: &str = "SERVER_MESSAGE";
    const SERVER_MODE_ENV_NAME: &str = "SERVER_MODE";
    const TLS_SERVER_CERT_ENV_NAME: &str = "TLS_SERVER_CERT";
    const TLS_SERVER_KEY_ENV_NAME: &str = "TLS_SERVER_KEY";
    const TLS_CLIENT_ROOTS_ENV_NAME: &str = "TLS_CLIENT_ROOTS";

    async fn new(config: GoServTlsConfig, client: Client) -> Self {
        let labels: Option<BTreeMap<String, String>> = Some(
            [
                ("app".to_string(), "go-serv".to_string()),
                (
                    TEST_RESOURCE_LABEL.0.to_string(),
                    TEST_RESOURCE_LABEL.1.to_string(),
                ),
            ]
            .into(),
        );
        let selector: Option<BTreeMap<String, String>> =
            Some([("app".to_string(), "go-serv".to_string())].into());

        let (guard, namespace) = ResourceGuard::create(
            Api::all(client.clone()),
            &Namespace {
                metadata: ObjectMeta {
                    name: Some(format!("go-serv-{:x}", rand::random::<u16>())),
                    labels: labels.clone(),
                    ..Default::default()
                },
                spec: None,
                status: None,
            },
            std::env::var_os(PRESERVE_FAILED_ENV_NAME).is_none(),
        )
        .await
        .unwrap();
        println!("CREATED A NAMESPACE: {namespace:?}");

        let service = Api::namespaced(client.clone(), namespace.metadata.name.as_deref().unwrap())
            .create(
                &Default::default(),
                &Service {
                    metadata: ObjectMeta {
                        name: Some("go-serv".into()),
                        labels: labels.clone(),
                        ..Default::default()
                    },
                    spec: Some(ServiceSpec {
                        type_: Some("NodePort".into()),
                        selector: selector.clone(),
                        ports: Some(vec![ServicePort {
                            app_protocol: Some("TCP".into()),
                            port: config.port.into(),
                            target_port: Some(IntOrString::Int(config.port.into())),
                            ..Default::default()
                        }]),
                        session_affinity: Some("None".into()),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        println!("CREATED A SERVICE: {service:?}");

        let secret = Api::namespaced(client.clone(), namespace.metadata.name.as_deref().unwrap())
            .create(
                &Default::default(),
                &Secret {
                    metadata: ObjectMeta {
                        name: Some("tls-certs".into()),
                        labels: labels.clone(),
                        ..Default::default()
                    },
                    string_data: Some(config.pem_files),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        println!("CREATED A SECRET: {secret:?}");

        let deployment =
            Api::namespaced(client.clone(), namespace.metadata.name.as_deref().unwrap())
                .create(
                    &Default::default(),
                    &Deployment {
                        metadata: ObjectMeta {
                            name: Some("go-serv".into()),
                            labels: labels.clone(),
                            ..Default::default()
                        },
                        spec: Some(DeploymentSpec {
                            replicas: Some(1),
                            selector: LabelSelector {
                                match_labels: selector.clone(),
                                ..Default::default()
                            },
                            template: PodTemplateSpec {
                                metadata: Some(ObjectMeta {
                                    labels: labels.clone(),
                                    ..Default::default()
                                }),
                                spec: Some(PodSpec {
                                    containers: vec![Self::prepare_container(
                                        config.port,
                                        config.env,
                                        "cert-volume",
                                    )],
                                    volumes: Some(vec![Volume {
                                        name: "cert-volume".into(),
                                        secret: Some(SecretVolumeSource {
                                            secret_name: secret.metadata.name.clone(),
                                            ..Default::default()
                                        }),
                                        ..Default::default()
                                    }]),
                                    ..Default::default()
                                }),
                            },
                            ..Default::default()
                        }),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
        println!("CREATED A DEPLOYMENT: {deployment:?}");

        println!("WAITING FOR AT LEAST ONE READY POD");
        let pods = watch::wait_until_pods_ready(&service, 1, client).await;
        println!("PODS READY: {pods:?}");

        Self {
            _guard: guard,

            namespace,
            deployment,
            service,
        }
    }

    fn prepare_container(
        port: u16,
        mut env: HashMap<String, String>,
        cert_volume_name: &str,
    ) -> Container {
        env.insert(Self::SERVER_PORT_ENV_NAME.into(), port.to_string());
        env.insert(
            Self::SERVER_MESSAGE_ENV_NAME.into(),
            Self::REMOTE_SERVER_RESPONSE.into(),
        );
        env.insert(Self::SERVER_MODE_ENV_NAME.into(), "HTTPS".into());

        let env = env
            .into_iter()
            .map(|(key, value)| EnvVar {
                name: key,
                value: Some(value),
                value_from: None,
            })
            .collect();

        let probe = Probe {
            tcp_socket: Some(TCPSocketAction {
                port: IntOrString::Int(port.into()),
                ..Default::default()
            }),
            initial_delay_seconds: Some(1),
            period_seconds: Some(2),
            failure_threshold: Some(3),
            ..Default::default()
        };

        Container {
            name: "go-serv".into(),
            command: Some(vec!["/app/main".into()]),
            env: Some(env),
            image: Some(Self::IMAGE_URL.into()),
            image_pull_policy: Some("IfNotPresent".into()),
            ports: Some(vec![ContainerPort {
                container_port: port.into(),
                protocol: Some("TCP".into()),
                ..Default::default()
            }]),
            volume_mounts: Some(vec![VolumeMount {
                name: cert_volume_name.into(),
                mount_path: GoServTlsConfig::MOUNT_PATH.into(),
                read_only: Some(true),
                ..Default::default()
            }]),
            startup_probe: Some(probe.clone()),
            liveness_probe: Some(probe.clone()),
            readiness_probe: Some(probe.clone()),
            ..Default::default()
        }
    }

    fn target_path(&self) -> String {
        format!(
            "deploy/{}/container/{}",
            self.deployment.metadata.name.as_ref().unwrap(),
            self.deployment
                .spec
                .as_ref()
                .unwrap()
                .template
                .spec
                .as_ref()
                .unwrap()
                .containers
                .first()
                .unwrap()
                .name
        )
    }
}

/// TLS config for the [`GoServService`].
struct GoServTlsConfig {
    /// Port on which the service should listen.
    port: u16,
    /// Generated PEMs by file name.
    ///
    /// When creating the [`Container`] for the service,
    /// you need to put all of these in the [`Self::MOUNT_PATH`] directory.
    /// [`Self::env`] relies on this.
    pem_files: BTreeMap<String, String>,
    /// Environment for the service.
    env: HashMap<String, String>,
}

impl GoServTlsConfig {
    /// Path where the certs folder should be mounted in the test service container.
    const MOUNT_PATH: &str = "/certs";
}

/// Generates:
/// 1. Configuration for the [`GoServService`] in mTLS mode
/// 2. [`StealPortTlsConfig`] to configure TLS stealing via the operator
/// 3. Root certificate for [`GoServService`] clients
fn generate_tls_configs() -> (GoServTlsConfig, StealPortTlsConfig, CertifiedKey) {
    let mut pem_files: BTreeMap<String, String> = Default::default();
    let mut env: HashMap<String, String> = Default::default();

    let root = generate_cert("test.root", None, true).unwrap();
    pem_files.insert(
        "root.cert.pem".into(),
        pem::encode_config(
            &Pem::new("CERTIFICATE", root.cert.der().to_vec()),
            EncodeConfig::new().set_line_ending(LineEnding::LF),
        ),
    );
    env.insert(
        GoServService::TLS_CLIENT_ROOTS_ENV_NAME.into(),
        format!("{}/root.cert.pem", GoServTlsConfig::MOUNT_PATH),
    );

    let server_cert = generate_cert("go-serv", Some(&root), false).unwrap();
    pem_files.insert(
        "server.cert.pem".into(),
        pem::encode_many_config(
            &[
                Pem::new("CERTIFICATE", server_cert.cert.der().to_vec()),
                Pem::new("CERTIFICATE", root.cert.der().to_vec()),
            ],
            EncodeConfig::new().set_line_ending(LineEnding::LF),
        ),
    );
    env.insert(
        GoServService::TLS_SERVER_CERT_ENV_NAME.into(),
        format!("{}/server.cert.pem", GoServTlsConfig::MOUNT_PATH),
    );
    pem_files.insert(
        "server.key.pem".into(),
        pem::encode_config(
            &Pem::new("PRIVATE KEY", server_cert.key_pair.serialize_der()),
            EncodeConfig::new().set_line_ending(LineEnding::LF),
        ),
    );
    env.insert(
        GoServService::TLS_SERVER_KEY_ENV_NAME.into(),
        format!("{}/server.key.pem", GoServTlsConfig::MOUNT_PATH),
    );

    let agent_client_cert = generate_cert("mirrord-agent", Some(&root), false).unwrap();
    pem_files.insert(
        "agent.cert.pem".into(),
        pem::encode_many_config(
            &[
                Pem::new("CERTIFICATE", agent_client_cert.cert.der().to_vec()),
                Pem::new("CERTIFICATE", root.cert.der().to_vec()),
            ],
            EncodeConfig::new().set_line_ending(LineEnding::LF),
        ),
    );
    pem_files.insert(
        "agent.key.pem".into(),
        pem::encode_config(
            &Pem::new("PRIVATE KEY", agent_client_cert.key_pair.serialize_der()),
            EncodeConfig::new().set_line_ending(LineEnding::LF),
        ),
    );

    let tls_steal_config = StealPortTlsConfig {
        port: 80,
        agent_as_server: AgentServerConfig {
            authentication: TlsAuthentication {
                cert_pem: env
                    .get(GoServService::TLS_SERVER_CERT_ENV_NAME)
                    .unwrap()
                    .into(),
                key_pem: env
                    .get(GoServService::TLS_SERVER_KEY_ENV_NAME)
                    .unwrap()
                    .into(),
            },
            alpn_protocols: ["h2", "http/1.1", "http/1.0"]
                .into_iter()
                .map(ToString::to_string)
                .collect(),
            verification: Some(TlsClientVerification {
                allow_anonymous: false,
                accept_any_cert: false,
                trust_roots: vec![env
                    .get(GoServService::TLS_CLIENT_ROOTS_ENV_NAME)
                    .unwrap()
                    .into()],
            }),
        },
        agent_as_client: AgentClientConfig {
            authentication: Some(TlsAuthentication {
                cert_pem: format!("{}/agent.cert.pem", GoServTlsConfig::MOUNT_PATH).into(),
                key_pem: format!("{}/agent.key.pem", GoServTlsConfig::MOUNT_PATH).into(),
            }),
            verification: TlsServerVerification {
                accept_any_cert: false,
                trust_roots: vec![env
                    .get(GoServService::TLS_CLIENT_ROOTS_ENV_NAME)
                    .unwrap()
                    .into()],
            },
        },
    };

    let go_serv_config = GoServTlsConfig {
        port: 80,
        pem_files,
        env,
    };

    (go_serv_config, tls_steal_config, root)
}

/// Test HTTPS client used in [`steal_tls_with_filter`].
struct TestClient {
    client: reqwest::Client,
    http1: bool,
}

impl TestClient {
    fn new(
        addr: SocketAddr,
        trusted_root_pem: &str,
        cert_and_key_pem: Option<&str>,
        http1: bool,
    ) -> Self {
        let mut builder = reqwest::ClientBuilder::new()
            .tls_built_in_root_certs(false)
            .add_root_certificate(
                reqwest::tls::Certificate::from_pem(trusted_root_pem.as_bytes()).unwrap(),
            )
            .resolve("go-serv", addr)
            .https_only(true);

        if let Some(cert_and_key_pem) = cert_and_key_pem {
            builder = builder
                .identity(reqwest::tls::Identity::from_pem(cert_and_key_pem.as_bytes()).unwrap());
        }

        if http1 {
            // If we don't set this, reqwest will use HTTP/2.
            // This is because the agent in the test is configured to prefer HTTP/2 in ALPN.
            builder = builder.http1_only();
        }

        Self {
            client: builder.build().unwrap(),
            http1,
        }
    }

    /// Makes a request to the test service and verifies that it is stolen/ignored.
    async fn make_request(&self, should_be_stolen: bool) {
        println!(
            "MAKING A REQUEST: should_be_stolen={should_be_stolen} http1={}",
            self.http1
        );

        let response = self
            .client
            .get("https://go-serv/")
            .header("x-steal", if should_be_stolen { "yes" } else { "no" })
            .header("host", "go-serv")
            .send()
            .await
            .unwrap();
        let status = response.status();
        println!("RESPONSE STATUS: {status}");

        let body = response.bytes().await.map(Vec::from).map(String::from_utf8);

        assert_eq!(
            status,
            reqwest::StatusCode::OK,
            "REQUEST FAILED: body=({body:?})"
        );

        if should_be_stolen {
            assert_eq!(body.unwrap().unwrap(), "GET");
        } else {
            assert_eq!(
                body.unwrap().unwrap(),
                GoServService::REMOTE_SERVER_RESPONSE
            );
        }
    }
}

/// Tests filtered TLS stealing.
///
/// Verifies:
/// * both HTTP/1 and HTTP/2
/// * both TLS and mTLS
#[rstest]
#[case::with_mtls(true)]
#[case::without_mtls(false)]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[timeout(Duration::from_secs(240))]
async fn steal_tls_with_filter(#[future] kube_client: Client, #[case] mtls: bool) {
    let client = kube_client.await;

    let (mut go_serv_config, mut steal_config, root) = generate_tls_configs();
    if !mtls {
        go_serv_config
            .env
            .remove(GoServService::TLS_CLIENT_ROOTS_ENV_NAME);
        steal_config.agent_as_server.verification = None;
        steal_config.agent_as_client.authentication = None;
    }

    let service = GoServService::new(go_serv_config, client.clone()).await;
    let steal_config = MirrordTlsStealConfig::new(
        "tls-steal-from-go-serv",
        MirrordTlsStealConfigSpec {
            target_path: Some(service.target_path()),
            selector: None,
            ports: vec![steal_config],
        },
    );
    let api = Api::<MirrordTlsStealConfig>::namespaced(
        client.clone(),
        service.namespace.metadata.name.as_deref().unwrap(),
    );
    let incluster_config = api
        .create(&Default::default(), &steal_config)
        .await
        .unwrap();

    let portforwarder = PortForwarder::new_for_service(client, &service.service, 80).await;
    let (test_client_http1, test_client_http2) = {
        let trusted_root_pem = pem::encode_config(
            &Pem::new("CERTIFICATE", root.cert.der().to_vec()),
            EncodeConfig::new().set_line_ending(LineEnding::LF),
        );

        let client_cert_and_key = mtls.then(|| {
            let client_cert = generate_cert("test-client", Some(&root), false).unwrap();
            pem::encode_many_config(
                &[
                    Pem::new("CERTIFICATE", client_cert.cert.der().to_vec()),
                    Pem::new("CERTIFICATE", root.cert.der().to_vec()),
                    Pem::new("PRIVATE KEY", client_cert.key_pair.serialize_der()),
                ],
                EncodeConfig::new().set_line_ending(LineEnding::LF),
            )
        });

        (
            TestClient::new(
                portforwarder.address(),
                &trusted_root_pem,
                client_cert_and_key.as_deref(),
                true,
            ),
            TestClient::new(
                portforwarder.address(),
                &trusted_root_pem,
                client_cert_and_key.as_deref(),
                false,
            ),
        )
    };

    let dir = tempfile::tempdir().unwrap();

    let local_server_cert = generate_cert("local.server", None, false).unwrap();
    let local_server_cert_path = dir.path().join("cert.pem");
    let local_server_cert_pem = pem::encode_config(
        &Pem::new("CERTIFICATE", local_server_cert.cert.der().to_vec()),
        EncodeConfig::new().set_line_ending(LineEnding::LF),
    );
    tokio::fs::write(&local_server_cert_path, local_server_cert_pem)
        .await
        .unwrap();
    let local_server_key_path = dir.path().join("key.pem");
    let local_server_key_pem = pem::encode_config(
        &Pem::new("PRIVATE KEY", local_server_cert.key_pair.serialize_der()),
        EncodeConfig::new().set_line_ending(LineEnding::LF),
    );
    tokio::fs::write(&local_server_key_path, local_server_key_pem)
        .await
        .unwrap();

    let config_path = dir.path().join("mirrord.json");
    let config = serde_json::json!({
        "feature": {
            "fs": "local",
            "network": {
                "incoming": {
                    "mode": "steal",
                    "http_filter": {
                        "header_filter": "x-steal: yes",
                        "ports": [incluster_config.spec.ports.first().unwrap().port],
                    },
                    "https_delivery": {
                        "protocol": "tls",
                        "server_cert": local_server_cert_path,
                    },
                },
                "outgoing": false,
                "dns": false,
            },
            "env": false,
            "hostname": false,
        }
    });
    tokio::fs::write(&config_path, serde_json::to_vec(&config).unwrap())
        .await
        .unwrap();

    let test_process_env = [
        ("SERVER_TLS_CERT", local_server_cert_path.to_str().unwrap()),
        ("SERVER_TLS_KEY", local_server_key_path.to_str().unwrap()),
    ]
    .into();

    println!("SPAWNING TEST PROCESS");
    let test_process = Application::Go23HTTP
        .run(
            &service.target_path(),
            service.namespace.metadata.name.as_deref(),
            Some(vec!["-f", config_path.to_str().unwrap()]),
            Some(test_process_env),
        )
        .await;

    test_process
        .wait_for_line(Duration::from_secs(120), "daemon subscribed")
        .await;
    println!("TEST PROCESS MADE A PORT SUBSCRIPTION");

    test_client_http1.make_request(false).await;
    test_client_http1.make_request(true).await;
    test_client_http1.make_request(false).await;
    test_client_http1.make_request(true).await;

    test_client_http2.make_request(false).await;
    test_client_http2.make_request(true).await;
    test_client_http2.make_request(false).await;
    test_client_http2.make_request(true).await;
}
