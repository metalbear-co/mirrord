#![cfg(feature = "operator")]

use std::{
    collections::{BTreeMap, HashMap},
    fmt,
};

use k8s_openapi::{
    api::{
        apps::v1::{Deployment, DeploymentSpec},
        core::v1::{
            Container, ContainerPort, EnvVar, Namespace, NamespaceSpec, PodSpec, PodTemplateSpec,
            Probe, Secret, SecretVolumeSource, Service, ServicePort, ServiceSpec, TCPSocketAction,
            Volume, VolumeMount,
        },
    },
    apimachinery::pkg::{apis::meta::v1::LabelSelector, util::intstr::IntOrString},
};
use kube::{api::ObjectMeta, Api, Client, Resource};
use mirrord_agent_env::steal_tls::StealPortTlsConfig;
use mirrord_operator::crd::steal_tls::MirrordTlsStealConfig;
use rcgen::{
    BasicConstraints, CertificateParams, CertifiedKey, DnType, DnValue, IsCa, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use serde::{de::DeserializeOwned, Serialize};

use crate::{
    cli,
    utils::{ResourceGuard, PRESERVE_FAILED_ENV_NAME, TEST_RESOURCE_LABEL},
};

struct GoServSetup {
    namespace: String,
    namespace_guard: ResourceGuard,

    deployment: Deployment,
    service: Service,
    secret: Secret,
}

impl GoServSetup {
    const IMAGE_URL: &str = "ghcr.io/metalbear-co/mirrord-go-server";
    const SERVER_RESPONSE: &str = "HELLO FROM REMOTE";

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

        let namespace = format!("go-serv-{:x}", rand::random::<u16>());
        let namespace_guard = ResourceGuard::create(
            Api::all(client.clone()),
            &Namespace {
                metadata: ObjectMeta {
                    name: Some(namespace.clone()),
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

        let service = Api::namespaced(client.clone(), &namespace)
            .create(
                &Default::default(),
                &Service {
                    metadata: ObjectMeta {
                        name: Some("go-serv".into()),
                        labels: labels.clone(),
                        ..Default::default()
                    },
                    spec: Some(ServiceSpec {
                        type_: Some("ClusterIP".into()),
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

        let secret = Self::create_secret(config.secret_entries, client.clone(), &namespace).await;

        let deployment = Api::namespaced(client.clone(), &namespace)
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
                                    &config.go_serv_tls_server_cert,
                                    &config.go_serv_tls_server_key,
                                    config.go_serv_tls_client_roots.as_deref(),
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

        Self {
            namespace,
            namespace_guard,
            deployment,
            service,
            secret,
        }
    }

    async fn create_secret(
        secret_entries: BTreeMap<String, String>,
        client: Client,
        namespace: &str,
    ) -> Secret {
        let secret = Secret {
            metadata: ObjectMeta {
                name: Some("tls-certs".into()),
                ..Default::default()
            },
            string_data: Some(secret_entries),
            ..Default::default()
        };

        let api = Api::<Secret>::namespaced(client, namespace);
        api.create(&Default::default(), &secret).await.unwrap()
    }

    fn prepare_container(
        port: u16,
        server_cert: &str,
        server_key: &str,
        client_roots: Option<&str>,
        cert_volume_name: &str,
    ) -> Container {
        let mut env = vec![
            EnvVar {
                name: "SERVER_PORT".into(),
                value: Some(port.to_string()),
                ..Default::default()
            },
            EnvVar {
                name: "SERVER_MESSAGE".into(),
                value: Some(Self::SERVER_RESPONSE.into()),
                ..Default::default()
            },
            EnvVar {
                name: "TLS_SERVER_CERT".into(),
                value: Some(format!("/certs/{server_cert}")),
                ..Default::default()
            },
            EnvVar {
                name: "TLS_SERVER_KEY".into(),
                value: Some(format!("/certs/{server_key}")),
                ..Default::default()
            },
        ];

        if let Some(client_roots) = client_roots {
            env.push(EnvVar {
                name: "TLS_CLIENT_ROOTS".into(),
                value: Some(format!("/certs/{client_roots}")),
                ..Default::default()
            })
        }

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
                mount_path: "/certs".into(),
                read_only: Some(true),
                ..Default::default()
            }]),
            startup_probe: Some(probe.clone()),
            liveness_probe: Some(probe.clone()),
            readiness_probe: Some(probe.clone()),
            ..Default::default()
        }
    }
}

struct GoServTlsConfig {
    secret_entries: BTreeMap<String, String>,
    port: u16,
    go_serv_tls_server_cert: String,
    go_serv_tls_server_key: String,
    go_serv_tls_client_roots: Option<String>,
}

impl GoServTlsConfig {
    /// Generates a new [`CertifiedKey`] with a random [`KeyPair`].
    fn generate_cert(
        name: String,
        issuer: Option<&CertifiedKey>,
        can_sign_others: bool,
    ) -> CertifiedKey {
        let key_pair = KeyPair::generate().unwrap();

        let mut params = CertificateParams::new(vec![name.clone()]).unwrap();
        params
            .distinguished_name
            .push(DnType::CommonName, DnValue::Utf8String(name.clone()));

        if can_sign_others {
            params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
            params.key_usages = vec![KeyUsagePurpose::KeyCertSign];
        }

        let cert = match issuer {
            Some(issuer) => params
                .signed_by(&key_pair, &issuer.cert, &issuer.key_pair)
                .unwrap(),
            None => params.self_signed(&key_pair).unwrap(),
        };

        CertifiedKey { cert, key_pair }
    }
}
