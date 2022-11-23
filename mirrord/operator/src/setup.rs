use std::{collections::BTreeMap, convert::Infallible, io::Write, str::FromStr};

use k8s_openapi::{
    api::{
        apps::v1::{Deployment, DeploymentSpec},
        core::v1::{
            Container, ContainerPort, EnvVar, Namespace, PodSpec, PodTemplateSpec, ServiceAccount,
        },
        rbac::v1::{ClusterRole, ClusterRoleBinding, PolicyRule, RoleRef, Subject},
    },
    apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta},
};
use thiserror::Error;

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
    service_account: OperatorServiceAccount,
    role: OperatorRole,
    role_binding: OperatorRoleBinding,
}

impl Operator {
    pub fn new(namespace: OperatorNamespace) -> Self {
        let deployment = OperatorDeployment::new(&namespace);
        let service_account = OperatorServiceAccount::new(&namespace);
        let role = OperatorRole::new();
        let role_binding = OperatorRoleBinding::new(&namespace, &role);

        Operator {
            namespace,
            deployment,
            service_account,
            role,
            role_binding,
        }
    }
}

impl OperatorSetup for Operator {
    fn to_writer<W: Write>(&self, mut writer: W) -> Result<()> {
        self.namespace.to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        self.service_account.to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        self.role.to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        self.role_binding.to_writer(&mut writer)?;

        writer.write_all(b"---\n")?;
        self.deployment.to_writer(&mut writer)?;

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
    pub fn new(namespace: &OperatorNamespace) -> Self {
        let app_labels = BTreeMap::from([("app".to_owned(), "operator".to_owned())]);

        let container = Container {
            name: "operator".to_owned(),
            image: Some("ghcr.io/metalbear-co/operator:latest".to_owned()),
            image_pull_policy: Some("IfNotPresent".to_owned()),
            env: Some(vec![EnvVar {
                name: "RUST_LOG".to_owned(),
                value: Some("mirrord=info,operator=info".to_owned()),
                value_from: None,
            }]),
            ports: Some(vec![ContainerPort {
                name: Some("tcp".to_owned()),
                container_port: 8080,
                ..Default::default()
            }]),
            ..Default::default()
        };

        let pod_spec = PodSpec {
            containers: vec![container],
            service_account_name: Some("operator".to_owned()),
            ..Default::default()
        };

        let spec = DeploymentSpec {
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(app_labels.clone()),
                    ..Default::default()
                }),
                spec: Some(pod_spec),
            },
            selector: LabelSelector {
                match_labels: Some(app_labels.clone()),
                ..Default::default()
            },
            ..Default::default()
        };

        let namespace = Deployment {
            metadata: ObjectMeta {
                name: Some("operator".to_owned()),
                namespace: Some(namespace.name().to_owned()),
                labels: Some(app_labels),
                ..Default::default()
            },
            spec: Some(spec),
            ..Default::default()
        };

        OperatorDeployment(namespace)
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
        let app_labels = BTreeMap::from([("app".to_owned(), "operator".to_owned())]);

        let sa = ServiceAccount {
            metadata: ObjectMeta {
                name: Some("operator".to_owned()),
                namespace: Some(namespace.name().to_owned()),
                labels: Some(app_labels),
                ..Default::default()
            },
            ..Default::default()
        };

        OperatorServiceAccount(sa)
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
                name: Some("operator".to_owned()),
                ..Default::default()
            },
            rules: Some(vec![
                PolicyRule {
                    api_groups: Some(vec!["".to_owned(), "apps".to_owned(), "batch".to_owned()]),
                    resources: Some(vec![
                        "pods".to_owned(),
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

impl OperatorSetup for OperatorRole {
    fn to_writer<W: Write>(&self, mut writer: W) -> Result<()> {
        serde_yaml::to_writer(&mut writer, &self.0).map_err(SetupError::from)
    }
}

#[derive(Debug)]
pub struct OperatorRoleBinding(ClusterRoleBinding);

impl OperatorRoleBinding {
    pub fn new(namespace: &OperatorNamespace, role: &OperatorRole) -> Self {
        let role_binding = ClusterRoleBinding {
            metadata: ObjectMeta {
                name: Some("operator".to_owned()),
                ..Default::default()
            },
            role_ref: role.as_role_ref(),
            subjects: Some(vec![Subject {
                api_group: Some("".to_owned()),
                kind: "ServiceAccount".to_owned(),
                name: "operator".to_owned(),
                namespace: Some(namespace.name().to_owned()),
            }]),
        };

        OperatorRoleBinding(role_binding)
    }
}

impl OperatorSetup for OperatorRoleBinding {
    fn to_writer<W: Write>(&self, mut writer: W) -> Result<()> {
        serde_yaml::to_writer(&mut writer, &self.0).map_err(SetupError::from)
    }
}
