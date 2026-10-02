use std::{io, path::PathBuf};

use itertools::Itertools;
use miette::Diagnostic;
use mirrord_kube::error::KubeApiError;
use reqwest::StatusCode;
use thiserror::Error;

use crate::error::GENERAL_BUG;

/// Errors of the `mirrord operator install` and `mirrord operator uninstall` commands.
#[derive(Debug, Error, Diagnostic)]
pub(crate) enum OperatorInstallError {
    #[error("failed to load the kubeconfig")]
    KubeConfig(#[source] Box<KubeApiError>),

    #[error("failed to create a Kubernetes client")]
    KubeClient(#[source] Box<kube::Error>),

    #[error("failed to create an HTTP client")]
    HttpClient(#[source] reqwest::Error),

    #[error("failed to ask for confirmation")]
    #[diagnostic(help("Pass `--yes` to continue without confirmation."))]
    Prompt(#[source] inquire::InquireError),

    #[error("cancelled, the cluster was not changed")]
    Declined,

    #[error("interrupted")]
    #[diagnostic(help(
        "The command stopped before it completed. Run it again to see what to do next."
    ))]
    Interrupted,

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

    #[error("the operator Deployment in the manifest has no valid `helm.sh/chart` label")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    NoChartVersionLabel,

    #[error("the operator manifest has an object with an invalid `apiVersion` or `kind`")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    InvalidObjectType(#[source] kube::core::gvk::ParseGroupVersionError),

    #[error("object `{name}` in the operator manifest has no `apiVersion` or `kind`")]
    #[diagnostic(help("{GENERAL_BUG}"))]
    UntypedObject { name: String },

    #[error("the cluster does not serve {object}")]
    Discovery {
        object: String,
        #[source]
        source: Box<kube::Error>,
    },

    #[error("failed to check the cluster for an existing mirrord operator")]
    #[diagnostic(help(
        "Installing the operator requires permissions to read and create cluster-scoped \
        resources, such as APIServices, CustomResourceDefinitions and ClusterRoles."
    ))]
    Preflight(#[source] Box<kube::Error>),

    #[error("mirrord operator {version} is already installed in namespace `{namespace}`")]
    #[diagnostic(help(
        "Run `mirrord operator status` to see its details. To install it again, remove it \
        first with `mirrord operator uninstall{context_arg}`."
    ))]
    AlreadyInstalled {
        namespace: String,
        version: semver::Version,
        /// Gives the commands the kubecontext of the run, if it has a name.
        context_arg: String,
    },

    #[error(
        "a mirrord operator is registered in namespace `{namespace}`, but it is not responding"
    )]
    #[diagnostic(help(
        "Inspect it with `kubectl{context_arg} get pods -n {namespace}`. Fix it, or remove it \
        with `mirrord operator uninstall{context_arg}` before installing it again."
    ))]
    Unhealthy {
        namespace: String,
        /// Gives kubectl the kubecontext of the run, if it has a name.
        context_arg: String,
        #[source]
        source: Box<kube::Error>,
    },

    #[error(
        "found objects left over from an earlier mirrord operator installation:\n{}",
        bullet_list(objects)
    )]
    #[diagnostic(help(
        "Remove them with `mirrord operator uninstall{context_arg}` before installing the \
        operator again."
    ))]
    LeftoverObjects {
        objects: Vec<String>,
        context_arg: String,
    },

    #[error("namespace `{namespace}` already exists")]
    #[diagnostic(help(
        "The operator is installed into its own namespace, which the installation creates. If \
        nothing uses the namespace, delete it. To install the operator into a different \
        namespace, use the helm chart."
    ))]
    ExistingNamespace { namespace: String },

    #[error(
        "found objects that `mirrord operator install` did not create:\n{}",
        bullet_list(objects)
    )]
    #[diagnostic(help(
        "They belong to an operator that was installed in a different way. Remove it the same \
        way that it was installed."
    ))]
    ForeignObjects { objects: Vec<String> },

    #[error("failed to look up {object}")]
    Lookup {
        object: String,
        #[source]
        source: Box<kube::Error>,
    },

    #[error("the cluster rejected {object}")]
    #[diagnostic(help(
        "Installing the operator requires permissions to create cluster-scoped resources, such \
        as CustomResourceDefinitions and ClusterRoles."
    ))]
    Rejected {
        object: String,
        #[source]
        source: Box<kube::Error>,
    },

    #[error("the operator did not become ready within {} minutes", .timeout.as_secs() / 60)]
    #[diagnostic(help("Inspect it with `kubectl{context_arg} get pods -n {namespace}`."))]
    NotReady {
        namespace: String,
        context_arg: String,
        timeout: std::time::Duration,
        #[source]
        source: Box<kube::Error>,
    },

    #[error("failed to start a trial")]
    SignupRequest(#[source] reqwest::Error),

    #[error("too many trials were started from this network recently")]
    #[diagnostic(help("Try again later, or pass an existing API key with `--api-key`."))]
    SignupRateLimited,

    #[error("starting a trial is currently unavailable")]
    #[diagnostic(help("Try again later, or pass an existing API key with `--api-key`."))]
    SignupUnavailable,

    #[error("failed to start a trial, the server responded with {status}: {body}")]
    SignupFailed { status: StatusCode, body: String },

    #[error("the mirrord operator is managed by helm")]
    #[diagnostic(help("Remove it with `{uninstall}`."))]
    ManagedByHelm {
        /// The `helm uninstall` command that removes it from the same kubecontext.
        uninstall: String,
    },

    #[error("failed to delete {object}")]
    #[diagnostic(help(
        "Removing the operator requires permissions to delete cluster-scoped resources, such as \
        CustomResourceDefinitions and ClusterRoles."
    ))]
    Delete {
        object: String,
        #[source]
        source: Box<kube::Error>,
    },

    #[error("{remaining} objects that the operator finalizes were not removed in time")]
    #[diagnostic(help("Run the command again to retry."))]
    NotFinalized { remaining: usize },

    #[error(
        "these objects were not removed within {} minutes:\n{}",
        .timeout.as_secs() / 60,
        bullet_list(objects)
    )]
    #[diagnostic(help("Run the command again to retry."))]
    NotRemoved {
        objects: Vec<String>,
        timeout: std::time::Duration,
    },
}

/// Formats the descriptions of objects as a list with one object on each line.
fn bullet_list(objects: &[String]) -> String {
    objects
        .iter()
        .map(|object| format!("- {object}"))
        .join("\n")
}
