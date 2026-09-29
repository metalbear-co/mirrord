use std::{io, path::PathBuf};

use itertools::Itertools;
use miette::Diagnostic;
use mirrord_kube::error::KubeApiError;
use reqwest::StatusCode;
use thiserror::Error;

use crate::error::GENERAL_BUG;

/// Errors of the `mirrord operator install` command.
#[derive(Debug, Error, Diagnostic)]
pub(crate) enum OperatorInstallError {
    #[error("failed to load the kubeconfig")]
    KubeConfig(#[source] Box<KubeApiError>),

    #[error("failed to create a Kubernetes client")]
    KubeClient(#[source] Box<kube::Error>),

    #[error("failed to create an HTTP client")]
    HttpClient(#[source] reqwest::Error),

}
