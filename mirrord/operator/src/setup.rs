use std::{collections::BTreeMap, convert::Infallible, io::Write, str::FromStr, sync::LazyLock};

use k8s_openapi::{
    api::{
        admissionregistration::v1::MutatingWebhookConfiguration,
        apps::v1::{Deployment, DeploymentSpec},
        core::v1::{
            ConfigMap, Container, ContainerPort, EnvVar, HTTPGetAction, Namespace,
            PodSecurityContext, PodSpec, PodTemplate, PodTemplateSpec, Probe, ResourceRequirements,
            Secret, SecretVolumeSource, SecurityContext, Service, ServiceAccount, ServicePort,
            ServiceSpec, Sysctl, Volume, VolumeMount,
        },
        rbac::v1::{
            ClusterRole, ClusterRoleBinding, PolicyRule, Role, RoleBinding, RoleRef, Subject,
        },
    },
    apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceDefinition,
    apimachinery::pkg::{
        api::resource::Quantity,
        apis::meta::v1::{LabelSelector, ObjectMeta},
        util::intstr::IntOrString,
    },
    kube_aggregator::pkg::apis::apiregistration::v1::{
        APIService, APIServiceSpec, ServiceReference,
    },
};
use kube::{CustomResourceExt, Resource};
use thiserror::Error;

use crate::crd::{
    MirrordClusterOperatorUserCredential, MirrordOperatorCrd, MirrordSqsSession,
    MirrordWorkloadQueueRegistry, TargetCrd,
    kafka::{MirrordKafkaClientConfig, MirrordKafkaEphemeralTopic, MirrordKafkaTopicsConsumer},
    mysql_branching::MysqlBranchDatabase,
    patch::{MirrordClusterWorkloadPatch, MirrordClusterWorkloadPatchRequest},
    policy::{MirrordClusterPolicy, MirrordPolicy},
    profile::{MirrordClusterProfile, MirrordProfile},
    session::MirrordClusterSession,
    steal_tls::{MirrordClusterTlsStealConfig, MirrordTlsStealConfig},
};

pub static OPERATOR_NAME: &str = "mirrord-operator";
/// 443 is standard port for APIService, do not change this value
/// (will require users to add FW rules)
static OPERATOR_PORT: i32 = 443;
static OPERATOR_ROLE_NAME: &str = "mirrord-operator";
static OPERATOR_ROLE_BINDING_NAME: &str = "mirrord-operator";
static OPERATOR_CLUSTER_ROLE_NAME: &str = "mirrord-operator";
static OPERATOR_CLUSTER_ROLE_BINDING_NAME: &str = "mirrord-operator";
static OPERATOR_CLIENT_CA_ROLE_NAME: &str = "mirrord-operator-apiserver-authentication";
static OPERATOR_CLUSTER_USER_ROLE_NAME: &str = "mirrord-operator-user";
static OPERATOR_LICENSE_SECRET_NAME: &str = "mirrord-operator-license";
static OPERATOR_LICENSE_SECRET_FILE_NAME: &str = "license.pem";
static OPERATOR_LICENSE_SECRET_VOLUME_NAME: &str = "license-volume";
static OPERATOR_SERVICE_ACCOUNT_NAME: &str = "mirrord-operator";
static OPERATOR_SERVICE_NAME: &str = "mirrord-operator";

static APP_LABELS: LazyLock<BTreeMap<String, String>> =
    LazyLock::new(|| BTreeMap::from([("app".to_owned(), OPERATOR_NAME.to_owned())]));
static RESOURCE_REQUESTS: LazyLock<BTreeMap<String, Quantity>> = LazyLock::new(|| {
    BTreeMap::from([
        ("cpu".to_owned(), Quantity("100m".to_owned())),
        ("memory".to_owned(), Quantity("100Mi".to_owned())),
    ])
});

macro_rules! writer_impl {
    ($ident:ident) => {
        impl OperatorSetup for $ident {
            fn to_writer<W: Write>(&self, writer: W) -> Result<()> {
                serde_yaml::to_writer(writer, &self.0).map_err(SetupWriteError::from)
            }
        }
    };
    ($($rest:ident),+) => {
        $( writer_impl!($rest); )+
    }
}

/// General Operator Error
#[derive(Debug, Error)]
pub enum SetupWriteError {
    #[error(transparent)]
    YamlSerialization(#[from] serde_yaml::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

type Result<T, E = SetupWriteError> = std::result::Result<T, E>;

pub trait OperatorSetup {
    fn to_writer<W: Write>(&self, writer: W) -> Result<()>;
}

pub enum LicenseType {
    Online(String),
    Offline(String),
}

pub struct SetupOptions {
    pub license: LicenseType,
    pub namespace: OperatorNamespace,
    pub image: String,
    pub aws_role_arn: Option<String>,
    pub sqs_splitting: bool,
    pub kafka_splitting: bool,
    pub application_auto_pause: bool,
    pub stateful_sessions: bool,
    pub mysql_branching: bool,
}

#[derive(Debug)]
pub struct Operator {
    api_service: OperatorApiService,
    deployment: OperatorDeployment,
    license_secret: Option<OperatorLicenseSecret>,
    namespace: OperatorNamespace,
    cluster_role: OperatorClusterRole,
    cluster_role_binding: OperatorClusterRoleBinding,
    role: Option<OperatorRole>,
    role_binding: Option<OperatorRoleBinding>,
    service: OperatorService,
    service_account: OperatorServiceAccount,
    user_cluster_role: OperatorClusterUserRole,
    client_ca_role: OperatorClientCaRole,
    client_ca_role_binding: OperatorClientCaRoleBinding,
    sqs_splitting: bool,
    kafka_splitting: bool,
    stateful_sessions: bool,
    mysql_branching: bool,
}

impl Operator {
    pub fn new(options: SetupOptions) -> Self {
        let SetupOptions {
            license,
            namespace,
            image,
            aws_role_arn,
            sqs_splitting,
            kafka_splitting,
            application_auto_pause,
            stateful_sessions,
            mysql_branching,
        } = options;

        let (license_secret, license_key) = match license {
            LicenseType::Online(license_key) => (None, Some(license_key)),
            LicenseType::Offline(license) => {
                (Some(OperatorLicenseSecret::new(&license, &namespace)), None)
            }
        };

        let service_account = OperatorServiceAccount::new(&namespace, aws_role_arn);

        let cluster_role = OperatorClusterRole::new(OperatorClusterRoleOptions {
            sqs_splitting,
            kafka_splitting,
            application_auto_pause,
            stateful_sessions,
            mysql_branching,
        });
        let cluster_role_binding = OperatorClusterRoleBinding::new(&cluster_role, &service_account);
        let user_cluster_role =
            OperatorClusterUserRole::new(OperatorClusterUserRoleOptions { mysql_branching });

        let (role, role_binding) = kafka_splitting
            .then(|| {
                let role = OperatorRole::new(&namespace);
                let role_binding = OperatorRoleBinding::new(&role, &service_account, &namespace);
                (role, role_binding)
            })
            .unzip();

        let client_ca_role = OperatorClientCaRole::new();
        let client_ca_role_binding =
            OperatorClientCaRoleBinding::new(&client_ca_role, &service_account);

        let deployment = OperatorDeployment::new(
            &namespace,
            &service_account,
            license_secret.as_ref(),
            license_key,
            image,
            sqs_splitting,
            kafka_splitting,
            mysql_branching,
            application_auto_pause,
        );

        let service = OperatorService::new(&namespace);

        let api_service = OperatorApiService::new(&service);

        Operator {
            api_service,
            deployment,
            license_secret,
            namespace,
            cluster_role,
            cluster_role_binding,
            role,
            role_binding,
            service,
            service_account,
            user_cluster_role,
            client_ca_role,
            client_ca_role_binding,
            sqs_splitting,
            kafka_splitting,
            stateful_sessions,
            mysql_branching,
        }
    }
}

impl OperatorSetup for Operator {
    fn to_writer<W: Write>(&self, mut writer: W) -> Result<()> {
        self.namespace.to_writer(&mut writer)?;

        if let Some(secret) = self.license_secret.as_ref() {
            writer.write_all(b"---\n")?;
            secret.to_writer(&mut writer)?;
        }

        writer.write_all(b"---\n")?;
        self.service_account.to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        self.cluster_role.to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        self.user_cluster_role.to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        self.client_ca_role.to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        self.cluster_role_binding.to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        self.client_ca_role_binding.to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        self.deployment.to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        self.service.to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        self.api_service.to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        MirrordPolicy::crd().to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        MirrordClusterPolicy::crd().to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        MirrordTlsStealConfig::crd().to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        MirrordClusterTlsStealConfig::crd().to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        MirrordClusterProfile::crd().to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        MirrordProfile::crd().to_writer(&mut writer)?;

        if self.sqs_splitting {
            writer.write_all(b"---\n")?;
            MirrordWorkloadQueueRegistry::crd().to_writer(&mut writer)?;

            writer.write_all(b"---\n")?;
            MirrordSqsSession::crd().to_writer(&mut writer)?;
        }

        if self.kafka_splitting {
            writer.write_all(b"---\n")?;
            MirrordKafkaClientConfig::crd().to_writer(&mut writer)?;

            writer.write_all(b"---\n")?;
            MirrordKafkaEphemeralTopic::crd().to_writer(&mut writer)?;

            writer.write_all(b"---\n")?;
            MirrordKafkaTopicsConsumer::crd().to_writer(&mut writer)?;

            if let Some(role) = self.role.as_ref() {
                writer.write_all(b"---\n")?;
                role.to_writer(&mut writer)?;
            }

            if let Some(role_binding) = self.role_binding.as_ref() {
                writer.write_all(b"---\n")?;
                role_binding.to_writer(&mut writer)?;
            }
        }

        if self.stateful_sessions {
            writer.write_all(b"---\n")?;
            MirrordClusterSession::crd().to_writer(&mut writer)?;
        }

        if self.mysql_branching {
            writer.write_all(b"---\n")?;
            MysqlBranchDatabase::crd().to_writer(&mut writer)?;
        }

        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct OperatorNamespace(Namespace);

impl OperatorNamespace {
    pub fn name(&self) -> &str {
        self.0.metadata.name.as_deref().unwrap_or_default()
    }
}

impl FromStr for OperatorNamespace {
    type Err = Infallible;

    fn from_str(namespace: &str) -> Result<Self, Self::Err> {
        let namespace = Namespace {
            metadata: ObjectMeta {
                name: Some(namespace.to_owned()),
                ..Default::default()
            },
            ..Default::default()
        };

        Ok(OperatorNamespace(namespace))
    }
}

#[derive(Debug)]
pub struct OperatorDeployment(Deployment);

impl OperatorDeployment {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        namespace: &OperatorNamespace,
        sa: &OperatorServiceAccount,
        license_secret: Option<&OperatorLicenseSecret>,
        license_key: Option<String>,
        image: String,
        sqs_splitting: bool,
        kafka_splitting: bool,
        mysql_branching: bool,
        application_auto_pause: bool,
    ) -> Self {
        let mut envs = vec![
            EnvVar {
                name: "RUST_LOG".to_owned(),
                value: Some("mirrord=info,operator=info".to_owned()),
                value_from: None,
            },
            EnvVar {
                name: "OPERATOR_ADDR".to_owned(),
                value: Some(format!("0.0.0.0:{OPERATOR_PORT}")),
                value_from: None,
            },
            EnvVar {
                name: "OPERATOR_NAMESPACE".to_owned(),
                value: Some(namespace.name().to_owned()),
                value_from: None,
            },
            EnvVar {
                name: "OPERATOR_SERVICE_NAME".to_owned(),
                value: Some(OPERATOR_SERVICE_NAME.to_owned()),
                value_from: None,
            },
        ];

        let mut volumes = Vec::new();

        let mut volume_mounts = Vec::new();

        if let Some(license_secret) = license_secret {
            envs.push(EnvVar {
                name: "OPERATOR_LICENSE_PATH".to_owned(),
                value: Some(format!("/license/{OPERATOR_LICENSE_SECRET_FILE_NAME}")),
                value_from: None,
            });

            volumes.push(Volume {
                name: OPERATOR_LICENSE_SECRET_VOLUME_NAME.to_owned(),
                secret: Some(SecretVolumeSource {
                    secret_name: Some(license_secret.name().to_owned()),
                    ..Default::default()
                }),
                ..Default::default()
            });

            volume_mounts.push(VolumeMount {
                name: OPERATOR_LICENSE_SECRET_VOLUME_NAME.to_owned(),
                mount_path: "/license".to_owned(),
                ..Default::default()
            });
        }

        // For downloading and using CA.
        volumes.push(Volume {
            name: "tmp".to_string(),
            empty_dir: Some(Default::default()),
            ..Default::default()
        });
        volume_mounts.push(VolumeMount {
            mount_path: "/tmp".to_string(),
            name: "tmp".to_string(),
            ..Default::default()
        });

        if let Some(license_key) = license_key {
            envs.push(EnvVar {
                name: "OPERATOR_LICENSE_KEY".to_owned(),
                value: Some(license_key),
                value_from: None,
            });
        }

        if sqs_splitting {
            envs.push(EnvVar {
                name: "OPERATOR_SQS_SPLITTING".to_owned(),
                value: Some("true".to_string()),
                value_from: None,
            });
        }

        if kafka_splitting {
            envs.push(EnvVar {
                name: "OPERATOR_KAFKA_SPLITTING".into(),
                value: Some("true".into()),
                value_from: None,
            });
        }

        if mysql_branching {
            envs.push(EnvVar {
                name: "OPERATOR_MYSQL_BRANCHING".into(),
                value: Some("true".into()),
                value_from: None,
            });
        }

        if application_auto_pause {
            envs.push(EnvVar {
                name: "OPERATOR_APPLICATION_PAUSE_AUTO_SYNC".into(),
                value: Some("true".into()),
                value_from: None,
            });
        }

        let health_probe = Probe {
            http_get: Some(HTTPGetAction {
                path: Some("/health".to_owned()),
                port: IntOrString::Int(OPERATOR_PORT),
                scheme: Some("HTTPS".to_owned()),
                ..Default::default()
            }),
            period_seconds: Some(5),
            ..Default::default()
        };

        let container = Container {
            name: OPERATOR_NAME.to_owned(),
            image: Some(image),
            image_pull_policy: Some("IfNotPresent".to_owned()),
            env: Some(envs),
            ports: Some(vec![ContainerPort {
                name: Some("https".to_owned()),
                container_port: OPERATOR_PORT,
                ..Default::default()
            }]),
            volume_mounts: Some(volume_mounts),
            security_context: Some(SecurityContext {
                allow_privilege_escalation: Some(false),
                privileged: Some(false),
                run_as_user: Some(1000),
                run_as_non_root: Some(true),
                read_only_root_filesystem: Some(true),
                ..Default::default()
            }),
            resources: Some(ResourceRequirements {
                requests: Some(RESOURCE_REQUESTS.clone()),
                ..Default::default()
            }),
            readiness_probe: Some(health_probe.clone()),
            liveness_probe: Some(health_probe),
            ..Default::default()
        };

        let pod_spec = PodSpec {
            security_context: Some(PodSecurityContext {
                sysctls: Some(vec![Sysctl {
                    name: "net.ipv4.ip_unprivileged_port_start".to_owned(),
                    value: "443".to_owned(),
                }]),
                ..Default::default()
            }),
            containers: vec![container],
            service_account_name: Some(sa.name().to_owned()),
            volumes: Some(volumes),
            ..Default::default()
        };

        let spec = DeploymentSpec {
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(APP_LABELS.clone()),
                    ..Default::default()
                }),
                spec: Some(pod_spec),
            },
            selector: LabelSelector {
                match_labels: Some(APP_LABELS.clone()),
                ..Default::default()
            },
            ..Default::default()
        };

        let deployment = Deployment {
            metadata: ObjectMeta {
                name: Some(OPERATOR_NAME.to_owned()),
                namespace: Some(namespace.name().to_owned()),
                labels: Some(APP_LABELS.clone()),
                ..Default::default()
            },
            spec: Some(spec),
            ..Default::default()
        };

        OperatorDeployment(deployment)
    }
}

#[derive(Debug)]
pub struct OperatorServiceAccount(ServiceAccount);

impl OperatorServiceAccount {
    pub fn new(namespace: &OperatorNamespace, aws_role_arn: Option<String>) -> Self {
        let sa = ServiceAccount {
            metadata: ObjectMeta {
                name: Some(OPERATOR_SERVICE_ACCOUNT_NAME.to_owned()),
                namespace: Some(namespace.name().to_owned()),
                annotations: aws_role_arn
                    .map(|arn| BTreeMap::from([("eks.amazonaws.com/role-arn".to_string(), arn)])),
                labels: Some(APP_LABELS.clone()),
                ..Default::default()
            },
            ..Default::default()
        };

        OperatorServiceAccount(sa)
    }

    fn name(&self) -> &str {
        self.0.metadata.name.as_deref().unwrap_or_default()
    }

    fn as_subject(&self) -> Subject {
        Subject {
            api_group: Some("".to_owned()),
            kind: "ServiceAccount".to_owned(),
            name: self.0.metadata.name.clone().unwrap_or_default(),
            namespace: self.0.metadata.namespace.clone(),
        }
    }
}

#[derive(Debug, Default)]
pub struct OperatorClusterRoleOptions {
    pub sqs_splitting: bool,
    pub kafka_splitting: bool,
    pub application_auto_pause: bool,
    pub stateful_sessions: bool,
    pub mysql_branching: bool,
}

#[derive(Debug)]
pub struct OperatorClusterRole(ClusterRole);

impl OperatorClusterRole {
    pub fn new(options: OperatorClusterRoleOptions) -> Self {
        let mut rules = vec![
            PolicyRule {
                api_groups: Some(vec![
                    "".to_owned(),
                    "apps".to_owned(),
                    "batch".to_owned(),
                    "argoproj.io".to_owned(),
                ]),
                resources: Some(vec![
                    "nodes".to_owned(),
                    "pods".to_owned(),
                    "pods/log".to_owned(),
                    "pods/ephemeralcontainers".to_owned(),
                    "deployments".to_owned(),
                    "deployments/scale".to_owned(),
                    "rollouts".to_owned(),
                    "rollouts/scale".to_owned(),
                    "jobs".to_owned(),
                    "cronjobs".to_owned(),
                    "statefulsets".to_owned(),
                    "statefulsets/scale".to_owned(),
                    "services".to_owned(),
                    "replicasets".to_owned(),
                ]),
                verbs: vec!["get".to_owned(), "list".to_owned(), "watch".to_owned()],
                ..Default::default()
            },
            PolicyRule {
                api_groups: Some(vec!["apps".to_owned(), "argoproj.io".to_owned()]),
                resources: Some(vec![
                    "deployments/scale".to_owned(),
                    "rollouts/scale".to_owned(),
                    "statefulsets/scale".to_owned(),
                ]),
                verbs: vec!["patch".to_owned()],
                ..Default::default()
            },
            PolicyRule {
                api_groups: Some(vec!["".to_owned(), "batch".to_owned()]),
                resources: Some(vec!["jobs".to_owned(), "pods".to_owned()]),
                verbs: vec!["create".to_owned(), "delete".to_owned()],
                ..Default::default()
            },
            PolicyRule {
                api_groups: Some(vec!["".to_owned()]),
                resources: Some(vec!["pods/ephemeralcontainers".to_owned()]),
                verbs: vec!["update".to_owned()],
                ..Default::default()
            },
            PolicyRule {
                api_groups: Some(vec!["authorization.k8s.io".to_owned()]),
                resources: Some(vec!["subjectaccessreviews".to_owned()]),
                verbs: vec!["create".to_owned()],
                ..Default::default()
            },
            // Allow the operator to list+get+watch mirrord policies.
            PolicyRule {
                // Both namespaced and cluster-wide policies live in the same API group.
                api_groups: Some(vec![MirrordPolicy::group(&()).into_owned()]),
                resources: Some(vec![
                    MirrordPolicy::plural(&()).into_owned(),
                    MirrordClusterPolicy::plural(&()).into_owned(),
                ]),
                verbs: vec!["list".to_owned(), "get".to_owned(), "watch".to_owned()],
                ..Default::default()
            },
            // Allow for patching replicas and environment variables.
            PolicyRule {
                api_groups: Some(vec!["apps".to_owned()]),
                resources: Some(vec![
                    "deployments".to_owned(),
                    "statefulsets".to_owned(),
                    "replicasets".to_owned(),
                ]),
                verbs: vec!["patch".to_owned()],
                ..Default::default()
            },
            // Allow for patching replicas and environment variables.
            PolicyRule {
                api_groups: Some(vec!["argoproj.io".to_owned()]),
                resources: Some(vec!["rollouts".to_owned()]),
                verbs: vec!["patch".to_owned()],
                ..Default::default()
            },
            // Allow the operator to list+get TLS steal configurations.
            PolicyRule {
                api_groups: Some(vec![MirrordTlsStealConfig::group(&()).into_owned()]),
                resources: Some(vec![
                    MirrordTlsStealConfig::plural(&()).into_owned(),
                    MirrordClusterTlsStealConfig::plural(&()).into_owned(),
                ]),
                verbs: vec!["list".to_owned(), "get".to_owned()],
                ..Default::default()
            },
            // `PodTemplate`s can be used as `workloadRef`s in Rollouts.
            PolicyRule {
                api_groups: Some(vec![PodTemplate::group(&()).into_owned()]),
                resources: Some(vec![PodTemplate::plural(&()).into_owned()]),
                verbs: vec!["get".to_owned(), "list".to_owned(), "watch".to_owned()],
                ..Default::default()
            },
            PolicyRule {
                api_groups: Some(vec![MirrordClusterWorkloadPatch::group(&()).into_owned()]),
                resources: Some(vec![
                    MirrordClusterWorkloadPatch::plural(&()).into_owned(),
                    MirrordClusterWorkloadPatchRequest::plural(&()).into_owned(),
                    format!("{}/status", MirrordClusterWorkloadPatchRequest::plural(&())),
                ]),
                verbs: vec!["*".to_owned()],
                ..Default::default()
            },
            PolicyRule {
                api_groups: Some(vec![MutatingWebhookConfiguration::group(&()).into_owned()]),
                resources: Some(vec![MutatingWebhookConfiguration::plural(&()).into_owned()]),
                verbs: vec![
                    "delete".to_owned(),
                    "deletecollection".to_owned(),
                    "list".to_owned(),
                ],
                ..Default::default()
            },
        ];

        if options.kafka_splitting || options.sqs_splitting {
            rules.push(PolicyRule {
                api_groups: Some(vec![MutatingWebhookConfiguration::group(&()).into_owned()]),
                resources: Some(vec![MutatingWebhookConfiguration::plural(&()).into_owned()]),
                verbs: vec![
                    "create".to_owned(),
                    "get".to_owned(),
                    "update".to_owned(),
                    "patch".to_owned(),
                ],
                ..Default::default()
            });
        }

        if options.kafka_splitting {
            rules.extend([
                PolicyRule {
                    api_groups: Some(vec![MirrordKafkaEphemeralTopic::group(&()).into_owned()]),
                    resources: Some(vec![MirrordKafkaEphemeralTopic::plural(&()).into_owned()]),
                    verbs: ["get", "list", "watch", "create", "delete"]
                        .into_iter()
                        .map(String::from)
                        .collect(),
                    ..Default::default()
                },
                PolicyRule {
                    api_groups: Some(vec![MirrordKafkaClientConfig::group(&()).into_owned()]),
                    resources: Some(vec![MirrordKafkaClientConfig::plural(&()).into_owned()]),
                    verbs: ["get", "list", "watch"]
                        .into_iter()
                        .map(String::from)
                        .collect(),
                    ..Default::default()
                },
                PolicyRule {
                    api_groups: Some(vec![MirrordKafkaTopicsConsumer::group(&()).into_owned()]),
                    resources: Some(vec![MirrordKafkaTopicsConsumer::plural(&()).into_owned()]),
                    verbs: ["get", "list", "watch"]
                        .into_iter()
                        .map(String::from)
                        .collect(),
                    ..Default::default()
                },
            ]);
        }

        if options.sqs_splitting {
            rules.extend([
                // Allow the SQS controller to update queue registry status.
                PolicyRule {
                    api_groups: Some(vec![MirrordWorkloadQueueRegistry::group(&()).into_owned()]),
                    resources: Some(vec![format!(
                        "{}/status",
                        MirrordWorkloadQueueRegistry::plural(&())
                    )]),
                    verbs: vec![
                        // For setting the status in the SQS controller.
                        "update".to_owned(),
                    ],
                    ..Default::default()
                },
                // Allow the operator to list mirrord queue registries.
                PolicyRule {
                    api_groups: Some(vec![MirrordWorkloadQueueRegistry::group(&()).into_owned()]),
                    resources: Some(vec![MirrordWorkloadQueueRegistry::plural(&()).into_owned()]),
                    verbs: vec!["get".to_owned(), "list".to_owned(), "watch".to_owned()],
                    ..Default::default()
                },
                // Allow the operator to control mirrord SQS session objects.
                PolicyRule {
                    api_groups: Some(vec![MirrordSqsSession::group(&()).into_owned()]),
                    resources: Some(vec![MirrordSqsSession::plural(&()).into_owned()]),
                    verbs: vec![
                        "create".to_owned(),
                        "watch".to_owned(),
                        "list".to_owned(),
                        "get".to_owned(),
                        "delete".to_owned(),
                        "deletecollection".to_owned(),
                        "patch".to_owned(),
                    ],
                    ..Default::default()
                },
                // Allow the SQS controller to update SQS session status.
                PolicyRule {
                    api_groups: Some(vec![MirrordSqsSession::group(&()).into_owned()]),
                    resources: Some(vec![format!("{}/status", MirrordSqsSession::plural(&()))]),
                    verbs: vec![
                        // For setting the status in the SQS controller.
                        "update".to_owned(),
                    ],
                    ..Default::default()
                },
                // Allow the operator to fetch env values from configmaps.
                PolicyRule {
                    api_groups: Some(vec![ConfigMap::group(&()).into_owned()]),
                    resources: Some(vec![ConfigMap::plural(&()).into_owned()]),
                    verbs: vec!["get".to_owned(), "list".to_owned(), "watch".to_owned()],
                    ..Default::default()
                },
            ]);
        }

        if options.application_auto_pause {
            rules.push(PolicyRule {
                api_groups: Some(vec!["argoproj.io".to_owned()]),
                resources: Some(vec!["applications".to_owned()]),
                verbs: vec!["list".to_owned(), "get".to_owned(), "patch".to_owned()],
                ..Default::default()
            });
        }

        if options.stateful_sessions {
            rules.push(PolicyRule {
                api_groups: Some(vec![MirrordClusterSession::group(&()).into_owned()]),
                resources: Some(vec![MirrordClusterSession::plural(&()).into_owned()]),
                verbs: ["get", "list", "watch", "create", "delete"]
                    .into_iter()
                    .map(String::from)
                    .collect(),
                ..Default::default()
            });
        }

        if options.mysql_branching {
            rules.extend([
                PolicyRule {
                    api_groups: Some(vec![MysqlBranchDatabase::group(&()).into_owned()]),
                    resources: Some(vec![MysqlBranchDatabase::plural(&()).into_owned()]),
                    verbs: ["list", "get", "watch", "delete", "update", "patch"]
                        .into_iter()
                        .map(String::from)
                        .collect(),
                    ..Default::default()
                },
                PolicyRule {
                    api_groups: Some(vec![MysqlBranchDatabase::group(&()).into_owned()]),
                    resources: Some(vec![format!(
                        "{}/status",
                        MysqlBranchDatabase::plural(&()).into_owned()
                    )]),
                    verbs: ["get", "update", "patch"]
                        .into_iter()
                        .map(String::from)
                        .collect(),
                    ..Default::default()
                },
            ])
        }

        let role = ClusterRole {
            metadata: ObjectMeta {
                name: Some(OPERATOR_CLUSTER_ROLE_NAME.to_owned()),
                ..Default::default()
            },
            rules: Some(rules),
            ..Default::default()
        };

        OperatorClusterRole(role)
    }

    fn as_role_ref(&self) -> RoleRef {
        RoleRef {
            api_group: "rbac.authorization.k8s.io".to_owned(),
            kind: "ClusterRole".to_owned(),
            name: self.0.metadata.name.clone().unwrap_or_default(),
        }
    }
}

impl Default for OperatorClusterRole {
    fn default() -> Self {
        Self::new(OperatorClusterRoleOptions::default())
    }
}

#[derive(Debug)]
pub struct OperatorClusterRoleBinding(ClusterRoleBinding);

impl OperatorClusterRoleBinding {
    pub fn new(role: &OperatorClusterRole, sa: &OperatorServiceAccount) -> Self {
        let role_binding = ClusterRoleBinding {
            metadata: ObjectMeta {
                name: Some(OPERATOR_CLUSTER_ROLE_BINDING_NAME.to_owned()),
                ..Default::default()
            },
            role_ref: role.as_role_ref(),
            subjects: Some(vec![sa.as_subject()]),
        };

        OperatorClusterRoleBinding(role_binding)
    }
}

#[derive(Debug)]
pub struct OperatorRole(Role);

impl OperatorRole {
    pub fn new(namespace: &OperatorNamespace) -> Self {
        let rules = vec![
            // Allow the operator to fetch Secrets in the operator's namespace
            PolicyRule {
                api_groups: Some(vec!["".to_owned()]),
                resources: Some(vec!["secrets".to_owned()]),
                verbs: ["get", "list", "watch"]
                    .into_iter()
                    .map(String::from)
                    .collect(),
                ..Default::default()
            },
        ];

        let role = Role {
            metadata: ObjectMeta {
                name: Some(OPERATOR_ROLE_NAME.to_owned()),
                namespace: Some(namespace.name().to_owned()),
                ..Default::default()
            },
            rules: Some(rules),
        };

        Self(role)
    }

    fn as_role_ref(&self) -> RoleRef {
        RoleRef {
            api_group: "rbac.authorization.k8s.io".to_owned(),
            kind: "Role".to_owned(),
            name: self.0.metadata.name.clone().unwrap_or_default(),
        }
    }
}

#[derive(Debug)]
pub struct OperatorRoleBinding(RoleBinding);

impl OperatorRoleBinding {
    pub fn new(
        role: &OperatorRole,
        sa: &OperatorServiceAccount,
        namespace: &OperatorNamespace,
    ) -> Self {
        let role_binding = RoleBinding {
            metadata: ObjectMeta {
                name: Some(OPERATOR_ROLE_BINDING_NAME.to_owned()),
                namespace: Some(namespace.name().to_owned()),
                ..Default::default()
            },
            role_ref: role.as_role_ref(),
            subjects: Some(vec![sa.as_subject()]),
        };

        OperatorRoleBinding(role_binding)
    }
}

#[derive(Debug)]
pub struct OperatorLicenseSecret(Secret);

impl OperatorLicenseSecret {
    pub fn new(license: &str, namespace: &OperatorNamespace) -> Self {
        let secret = Secret {
            metadata: ObjectMeta {
                name: Some(OPERATOR_LICENSE_SECRET_NAME.to_owned()),
                namespace: Some(namespace.name().to_owned()),
                ..Default::default()
            },
            string_data: Some(BTreeMap::from([(
                OPERATOR_LICENSE_SECRET_FILE_NAME.to_owned(),
                license.to_owned(),
            )])),
            ..Default::default()
        };

        OperatorLicenseSecret(secret)
    }

    fn name(&self) -> &str {
        self.0.metadata.name.as_deref().unwrap_or_default()
    }
}

#[derive(Debug)]
pub struct OperatorService(Service);

impl OperatorService {
    pub fn new(namespace: &OperatorNamespace) -> Self {
        let service = Service {
            metadata: ObjectMeta {
                name: Some(OPERATOR_SERVICE_NAME.to_owned()),
                namespace: Some(namespace.name().to_owned()),
                labels: Some(APP_LABELS.clone()),
                ..Default::default()
            },
            spec: Some(ServiceSpec {
                type_: Some("ClusterIP".to_owned()),
                selector: Some(APP_LABELS.clone()),
                ports: Some(vec![ServicePort {
                    name: Some("https".to_owned()),
                    port: OPERATOR_PORT,
                    target_port: Some(IntOrString::String("https".to_owned())),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };

        OperatorService(service)
    }

    pub fn as_referance(&self) -> ServiceReference {
        ServiceReference {
            name: self.0.metadata.name.clone(),
            namespace: self.0.metadata.namespace.clone(),
            port: Some(OPERATOR_PORT),
        }
    }
}

#[derive(Debug)]
pub struct OperatorApiService(APIService);

impl OperatorApiService {
    pub fn new(service: &OperatorService) -> Self {
        let group = TargetCrd::group(&());
        let version = TargetCrd::version(&());

        let api_service = APIService {
            metadata: ObjectMeta {
                name: Some(format!("{version}.{group}")),
                ..Default::default()
            },
            spec: Some(APIServiceSpec {
                ca_bundle: None,
                group: Some(group.to_string()),
                group_priority_minimum: 1000,
                insecure_skip_tls_verify: Some(true),
                service: Some(service.as_referance()),
                version: Some(version.to_string()),
                version_priority: 15,
            }),
            ..Default::default()
        };

        OperatorApiService(api_service)
    }
}

#[derive(Debug, Default)]
pub struct OperatorClusterUserRoleOptions {
    pub mysql_branching: bool,
}

#[derive(Debug)]
pub struct OperatorClusterUserRole(ClusterRole);

impl OperatorClusterUserRole {
    pub fn new(options: OperatorClusterUserRoleOptions) -> Self {
        let mut rules = vec![
            PolicyRule {
                api_groups: Some(vec!["operator.metalbear.co".to_owned()]),
                resources: Some(vec![
                    "copytargets".to_owned(),
                    "mirrordoperators".to_owned(),
                    "targets".to_owned(),
                    "targets/port-locks".to_owned(),
                    MirrordOperatorCrd::plural(&()).into_owned(),
                ]),
                verbs: vec!["get".to_owned(), "list".to_owned()],
                ..Default::default()
            },
            PolicyRule {
                api_groups: Some(vec!["operator.metalbear.co".to_owned()]),
                resources: Some(vec![
                    "mirrordoperators/certificate".to_owned(),
                    "copytargets".to_owned(),
                ]),
                verbs: vec!["create".to_owned()],
                ..Default::default()
            },
            PolicyRule {
                api_groups: Some(vec!["operator.metalbear.co".to_owned()]),
                resources: Some(vec!["targets".to_owned(), "copytargets".to_owned()]),
                verbs: vec!["proxy".to_owned()],
                ..Default::default()
            },
            PolicyRule {
                api_groups: Some(vec!["operator.metalbear.co".to_owned()]),
                resources: Some(vec!["sessions".to_owned()]),
                verbs: vec!["deletecollection".to_owned(), "delete".to_owned()],
                ..Default::default()
            },
            PolicyRule {
                api_groups: Some(vec![MirrordClusterProfile::group(&()).into_owned()]),
                resources: Some(vec![MirrordClusterProfile::plural(&()).into_owned()]),
                verbs: vec!["list", "get"].into_iter().map(String::from).collect(),
                ..Default::default()
            },
            PolicyRule {
                api_groups: Some(vec![
                    MirrordClusterOperatorUserCredential::group(&()).into_owned(),
                ]),
                resources: Some(vec![
                    MirrordClusterOperatorUserCredential::plural(&()).into_owned(),
                ]),
                verbs: vec!["create".to_owned()],
                ..Default::default()
            },
        ];

        if options.mysql_branching {
            rules.extend([PolicyRule {
                api_groups: Some(vec![MysqlBranchDatabase::group(&()).into_owned()]),
                resources: Some(vec![MysqlBranchDatabase::plural(&()).into_owned()]),
                verbs: vec!["create", "list", "get", "watch"]
                    .into_iter()
                    .map(String::from)
                    .collect(),
                ..Default::default()
            }]);
        }

        let role = ClusterRole {
            metadata: ObjectMeta {
                name: Some(OPERATOR_CLUSTER_USER_ROLE_NAME.to_owned()),
                ..Default::default()
            },
            rules: Some(rules),
            ..Default::default()
        };

        OperatorClusterUserRole(role)
    }
}

impl Default for OperatorClusterUserRole {
    fn default() -> Self {
        Self::new(Default::default())
    }
}

#[derive(Debug)]
pub struct OperatorClientCaRole(Role);

impl OperatorClientCaRole {
    pub fn new() -> Self {
        let role = Role {
            metadata: ObjectMeta {
                name: Some(OPERATOR_CLIENT_CA_ROLE_NAME.to_owned()),
                namespace: Some("kube-system".to_owned()),
                ..Default::default()
            },
            rules: Some(vec![PolicyRule {
                api_groups: Some(vec!["".to_owned()]),
                resources: Some(vec!["configmaps".to_owned()]),
                verbs: vec!["get".to_owned()],
                resource_names: Some(vec!["extension-apiserver-authentication".to_owned()]),
                ..Default::default()
            }]),
        };

        OperatorClientCaRole(role)
    }

    fn as_role_ref(&self) -> RoleRef {
        RoleRef {
            api_group: "rbac.authorization.k8s.io".to_owned(),
            kind: "Role".to_owned(),
            name: self.0.metadata.name.clone().unwrap_or_default(),
        }
    }
}

impl Default for OperatorClientCaRole {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug)]
pub struct OperatorClientCaRoleBinding(RoleBinding);

impl OperatorClientCaRoleBinding {
    pub fn new(role: &OperatorClientCaRole, sa: &OperatorServiceAccount) -> Self {
        let role = RoleBinding {
            metadata: ObjectMeta {
                name: Some(OPERATOR_CLIENT_CA_ROLE_NAME.to_owned()),
                namespace: role.0.metadata.namespace.clone(),
                ..Default::default()
            },
            role_ref: role.as_role_ref(),
            subjects: Some(vec![sa.as_subject()]),
        };

        OperatorClientCaRoleBinding(role)
    }
}

impl OperatorSetup for CustomResourceDefinition {
    fn to_writer<W: Write>(&self, writer: W) -> Result<()> {
        serde_yaml::to_writer(writer, &self).map_err(SetupWriteError::from)
    }
}

writer_impl![
    OperatorNamespace,
    OperatorDeployment,
    OperatorServiceAccount,
    OperatorClusterRole,
    OperatorClusterRoleBinding,
    OperatorLicenseSecret,
    OperatorService,
    OperatorApiService,
    OperatorClusterUserRole,
    OperatorClientCaRole,
    OperatorClientCaRoleBinding,
    OperatorRole,
    OperatorRoleBinding
];
