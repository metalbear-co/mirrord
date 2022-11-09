use std::{collections::BTreeMap, convert::Infallible, io::Write, str::FromStr};

use k8s_openapi::{
    api::{
        apps::v1::{Deployment, DeploymentSpec},
        core::v1::{Container, Namespace, PodSpec, PodTemplateSpec, ServiceAccount},
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
}

impl Operator {
    pub fn new(namespace: OperatorNamespace) -> Self {
        let deployment = OperatorDeployment::new(&namespace);
        let service_account = OperatorServiceAccount::new(&namespace);

        Operator {
            namespace,
            deployment,
            service_account,
        }
    }
}

impl OperatorSetup for Operator {
    fn to_writer<W: Write>(&self, mut writer: W) -> Result<()> {
        self.namespace.to_writer(&mut writer)?;
        writer.write_all(b"---\n")?;
        self.service_account.to_writer(&mut writer)?;
        writer.write_all(b"---\n")?;
        self.deployment.to_writer(&mut writer)?;

        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct OperatorNamespace(Namespace);

impl OperatorNamespace {
    pub fn name(&self) -> String {
        self.0.metadata.name.clone().unwrap_or_default()
    }
}

impl Default for OperatorNamespace {
    fn default() -> Self {
        OperatorNamespace::from_str("mirrord").expect("infallible")
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
            ..Default::default()
        };

        let pod_spec = PodSpec {
            containers: vec![container],
            ..Default::default()
        };

        let spec = DeploymentSpec {
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(app_labels.clone()),
                    ..Default::default()
                }),
                spec: Some(pod_spec),
                ..Default::default()
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
                namespace: Some(namespace.name()),
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
                namespace: Some(namespace.name()),
                labels: Some(app_labels.clone()),
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
