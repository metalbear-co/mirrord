use std::{collections::BTreeMap, convert::Infallible, io::Write, str::FromStr, sync::LazyLock};

use k8s_openapi::{
    api::{
        apps::v1::{Deployment, DeploymentSpec},
        core::v1::{
            Container, ContainerPort, EnvFromSource, EnvVar, Namespace, PodSpec, PodTemplateSpec,
            ResourceRequirements, Secret, SecretEnvSource, SecretVolumeSource, SecurityContext,
            Service, ServiceAccount, ServicePort, ServiceSpec, Volume, VolumeMount,
        },
        rbac::v1::{ClusterRole, ClusterRoleBinding, PolicyRule, RoleRef, Subject},
    },
    apimachinery::pkg::{
        api::resource::Quantity,
        apis::meta::v1::{LabelSelector, ObjectMeta},
        util::intstr::IntOrString,
    },
    kube_aggregator::pkg::apis::apiregistration::v1::{
        APIService, APIServiceSpec, ServiceReference,
    },
};
use kube::Resource;
use thiserror::Error;

use crate::crd::TargetCrd;

static OPERATOR_NAME: &str = "mirrord-operator";
static OPERATOR_PORT: i32 = 3000;
static OPERATOR_ROLE_NAME: &str = "mirrord-operator";
static OPERATOR_ROLE_BINDING_NAME: &str = "mirrord-operator";
static OPERATOR_SECRET_NAME: &str = "mirrord-operator-license";
static OPERATOR_TLS_SECRET_NAME: &str = "mirrord-operator-tls";
static OPERATOR_TLS_VOLUME_NAME: &str = "tls-volume";
static OPERATOR_TLS_KEY_FILE_NAME: &str = "tls.key";
static OPERATOR_TLS_CERT_FILE_NAME: &str = "tls.pem";
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

/// General Operator Error
#[derive(Debug, Error)]
pub enum SetupError {
    #[error(transparent)]
    Reader(#[from] std::io::Error),
    #[error(transparent)]
    YamlSerialization(#[from] serde_yaml::Error),
}

type Result<T, E = SetupError> = std::result::Result<T, E>;

pub trait OperatorSetup {
    fn to_writer<W: Write>(&self, writer: W) -> Result<()>;
}

#[derive(Debug)]
pub struct Operator {
    namespace: OperatorNamespace,
    deployment: OperatorDeployment,
    role: OperatorRole,
    role_binding: OperatorRoleBinding,
    secret: OperatorSecret,
    service: OperatorService,
    service_account: OperatorServiceAccount,
    tls_secret: OperatorTlsSecret,
    api_service: OperatorApiService,
}

impl Operator {
    pub fn new(license_key: String, namespace: OperatorNamespace) -> Self {
        let secret = OperatorSecret::new(&license_key, &namespace);
        let service_account = OperatorServiceAccount::new(&namespace);

        let tls_secret = OperatorTlsSecret::new(&namespace);

        let role = OperatorRole::new();
        let role_binding = OperatorRoleBinding::new(&role, &service_account);

        let deployment =
            OperatorDeployment::new(&namespace, &service_account, &secret, &tls_secret);

        let service = OperatorService::new(&namespace);

        let api_service = OperatorApiService::new(&service);

        Operator {
            namespace,
            deployment,
            role,
            role_binding,
            secret,
            service,
            service_account,
            tls_secret,
            api_service,
        }
    }
}

impl OperatorSetup for Operator {
    fn to_writer<W: Write>(&self, mut writer: W) -> Result<()> {
        self.namespace.to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        self.secret.to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        self.service_account.to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        self.role.to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        self.role_binding.to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        self.deployment.to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        self.service.to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        self.tls_secret.to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        self.api_service.to_writer(&mut writer)?;

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

impl OperatorSetup for OperatorNamespace {
    fn to_writer<W: Write>(&self, mut writer: W) -> Result<()> {
        serde_yaml::to_writer(&mut writer, &self.0).map_err(SetupError::from)
    }
}

#[derive(Debug)]
pub struct OperatorDeployment(Deployment);

impl OperatorDeployment {
    pub fn new(
        namespace: &OperatorNamespace,
        sa: &OperatorServiceAccount,
        secret: &OperatorSecret,
        tls_secret: &OperatorTlsSecret,
    ) -> Self {
        let container = Container {
            name: OPERATOR_NAME.to_owned(),
            image: Some(format!(
                "ghcr.io/metalbear-co/operator:{}",
                env!("CARGO_PKG_VERSION")
            )),
            image_pull_policy: Some("IfNotPresent".to_owned()),
            env: Some(vec![
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
                    name: "OPERATOR_TLS_CERT_PATH".to_owned(),
                    value: Some(format!("/tls/{OPERATOR_TLS_CERT_FILE_NAME}")),
                    value_from: None,
                },
                EnvVar {
                    name: "OPERATOR_TLS_KEY_PATH".to_owned(),
                    value: Some(format!("/tls/{OPERATOR_TLS_KEY_FILE_NAME}")),
                    value_from: None,
                },
            ]),
            env_from: Some(vec![EnvFromSource {
                secret_ref: Some(SecretEnvSource {
                    name: Some(secret.name().to_owned()),
                    ..Default::default()
                }),
                ..Default::default()
            }]),
            ports: Some(vec![ContainerPort {
                name: Some("https".to_owned()),
                container_port: OPERATOR_PORT,
                ..Default::default()
            }]),
            volume_mounts: Some(vec![VolumeMount {
                name: OPERATOR_TLS_VOLUME_NAME.to_owned(),
                mount_path: "/tls".to_owned(),
                ..Default::default()
            }]),
            security_context: Some(SecurityContext {
                allow_privilege_escalation: Some(false),
                privileged: Some(false),
                run_as_user: Some(1001),
                ..Default::default()
            }),
            resources: Some(ResourceRequirements {
                requests: Some(RESOURCE_REQUESTS.clone()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let pod_spec = PodSpec {
            containers: vec![container],
            service_account_name: Some(sa.name().to_owned()),
            volumes: Some(vec![Volume {
                name: OPERATOR_TLS_VOLUME_NAME.to_owned(),
                secret: Some(SecretVolumeSource {
                    secret_name: Some(tls_secret.name().to_owned()),
                    ..Default::default()
                }),
                ..Default::default()
            }]),
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

impl OperatorSetup for OperatorDeployment {
    fn to_writer<W: Write>(&self, mut writer: W) -> Result<()> {
        serde_yaml::to_writer(&mut writer, &self.0).map_err(SetupError::from)
    }
}

#[derive(Debug)]
pub struct OperatorServiceAccount(ServiceAccount);

impl OperatorServiceAccount {
    pub fn new(namespace: &OperatorNamespace) -> Self {
        let sa = ServiceAccount {
            metadata: ObjectMeta {
                name: Some(OPERATOR_SERVICE_ACCOUNT_NAME.to_owned()),
                namespace: Some(namespace.name().to_owned()),
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

impl OperatorSetup for OperatorServiceAccount {
    fn to_writer<W: Write>(&self, mut writer: W) -> Result<()> {
        serde_yaml::to_writer(&mut writer, &self.0).map_err(SetupError::from)
    }
}

#[derive(Debug)]
pub struct OperatorRole(ClusterRole);

impl OperatorRole {
    pub fn new() -> Self {
        let role = ClusterRole {
            metadata: ObjectMeta {
                name: Some(OPERATOR_ROLE_NAME.to_owned()),
                ..Default::default()
            },
            rules: Some(vec![
                PolicyRule {
                    api_groups: Some(vec!["".to_owned(), "apps".to_owned(), "batch".to_owned()]),
                    resources: Some(vec![
                        "pods".to_owned(),
                        "pods/ephemeralcontainers".to_owned(),
                        "deployments".to_owned(),
                        "jobs".to_owned(),
                    ]),
                    verbs: vec!["get".to_owned(), "list".to_owned(), "watch".to_owned()],
                    ..Default::default()
                },
                PolicyRule {
                    api_groups: Some(vec!["batch".to_owned()]),
                    resources: Some(vec!["jobs".to_owned()]),
                    verbs: vec!["create".to_owned()],
                    ..Default::default()
                },
                PolicyRule {
                    api_groups: Some(vec!["".to_owned()]),
                    resources: Some(vec!["pods/ephemeralcontainers".to_owned()]),
                    verbs: vec!["update".to_owned()],
                    ..Default::default()
                },
                PolicyRule {
                    api_groups: Some(vec!["".to_owned(), "authentication.k8s.io".to_owned()]),
                    resources: Some(vec![
                        "groups".to_owned(),
                        "users".to_owned(),
                        "userextras/accesskeyid".to_owned(),
                        "userextras/arn".to_owned(),
                        "userextras/canonicalarn".to_owned(),
                        "userextras/sessionname".to_owned(),
                        "userextras/iam.gke.io/user-assertion".to_owned(),
                        "userextras/user-assertion.cloud.google.com".to_owned(),
                    ]),
                    verbs: vec!["impersonate".to_owned()],
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };

        OperatorRole(role)
    }

    fn as_role_ref(&self) -> RoleRef {
        RoleRef {
            api_group: "rbac.authorization.k8s.io".to_owned(),
            kind: "ClusterRole".to_owned(),
            name: self.0.metadata.name.clone().unwrap_or_default(),
        }
    }
}

impl Default for OperatorRole {
    fn default() -> Self {
        Self::new()
    }
}

impl OperatorSetup for OperatorRole {
    fn to_writer<W: Write>(&self, mut writer: W) -> Result<()> {
        serde_yaml::to_writer(&mut writer, &self.0).map_err(SetupError::from)
    }
}

#[derive(Debug)]
pub struct OperatorRoleBinding(ClusterRoleBinding);

impl OperatorRoleBinding {
    pub fn new(role: &OperatorRole, sa: &OperatorServiceAccount) -> Self {
        let role_binding = ClusterRoleBinding {
            metadata: ObjectMeta {
                name: Some(OPERATOR_ROLE_BINDING_NAME.to_owned()),
                ..Default::default()
            },
            role_ref: role.as_role_ref(),
            subjects: Some(vec![sa.as_subject()]),
        };

        OperatorRoleBinding(role_binding)
    }
}

impl OperatorSetup for OperatorRoleBinding {
    fn to_writer<W: Write>(&self, mut writer: W) -> Result<()> {
        serde_yaml::to_writer(&mut writer, &self.0).map_err(SetupError::from)
    }
}

#[derive(Debug)]
pub struct OperatorSecret(Secret);

impl OperatorSecret {
    pub fn new(license_key: &str, namespace: &OperatorNamespace) -> Self {
        let secret = Secret {
            metadata: ObjectMeta {
                name: Some(OPERATOR_SECRET_NAME.to_owned()),
                namespace: Some(namespace.name().to_owned()),
                ..Default::default()
            },
            string_data: Some(BTreeMap::from([(
                "OPERATOR_LICENSE_KEY".to_owned(),
                license_key.to_owned(),
            )])),
            ..Default::default()
        };

        OperatorSecret(secret)
    }

    fn name(&self) -> &str {
        self.0.metadata.name.as_deref().unwrap_or_default()
    }
}

impl OperatorSetup for OperatorSecret {
    fn to_writer<W: Write>(&self, mut writer: W) -> Result<()> {
        serde_yaml::to_writer(&mut writer, &self.0).map_err(SetupError::from)
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

impl OperatorSetup for OperatorService {
    fn to_writer<W: Write>(&self, mut writer: W) -> Result<()> {
        serde_yaml::to_writer(&mut writer, &self.0).map_err(SetupError::from)
    }
}

#[derive(Debug)]
pub struct OperatorTlsSecret(Secret);

impl OperatorTlsSecret {
    pub fn new(namespace: &OperatorNamespace) -> Self {
        let cert = rcgen::generate_simple_self_signed(vec![
            OPERATOR_SERVICE_NAME.to_owned(),
            format!("{OPERATOR_SERVICE_NAME}.svc.cluster.local"),
            format!(
                "{OPERATOR_SERVICE_NAME}.{}.svc.cluster.local",
                namespace.name()
            ),
        ])
        .expect("unable to create self signed certificate");

        let secret = Secret {
            metadata: ObjectMeta {
                name: Some(OPERATOR_TLS_SECRET_NAME.to_owned()),
                namespace: Some(namespace.name().to_owned()),
                ..Default::default()
            },
            string_data: Some(BTreeMap::from([
                (
                    OPERATOR_TLS_KEY_FILE_NAME.to_owned(),
                    cert.get_key_pair().serialize_pem(),
                ),
                (
                    OPERATOR_TLS_CERT_FILE_NAME.to_owned(),
                    cert.serialize_pem().unwrap(),
                ),
            ])),
            ..Default::default()
        };

        OperatorTlsSecret(secret)
    }

    fn name(&self) -> &str {
        self.0.metadata.name.as_deref().unwrap_or_default()
    }
}

impl OperatorSetup for OperatorTlsSecret {
    fn to_writer<W: Write>(&self, mut writer: W) -> Result<()> {
        serde_yaml::to_writer(&mut writer, &self.0).map_err(SetupError::from)
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

impl OperatorSetup for OperatorApiService {
    fn to_writer<W: Write>(&self, mut writer: W) -> Result<()> {
        serde_yaml::to_writer(&mut writer, &self.0).map_err(SetupError::from)
    }
}
