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

    #[error("failed to fetch `{url}`")]
    Fetch {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("the charts index at `{url}` does not list any mirrord-operator chart")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    NoChartVersion { url: String },

    #[error("failed to parse the charts index")]
    ParseIndex(#[source] serde_saphyr::Error),

    #[error("mirrord-operator chart {version} is still being published")]
    #[diagnostic(help("Try again in a minute."))]
    ManifestNotPublished { version: semver::Version },

    #[error("failed to read the operator manifest from `{}`", path.display())]
    ReadManifest {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to parse the operator manifest")]
    ParseManifest(#[source] serde_saphyr::Error),

    #[error("expected the API key placeholder exactly once in the operator manifest, found {0}")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    ApiKeyPlaceholder(usize),

    #[error("the operator manifest has no Deployment")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    NoDeployment,

}
