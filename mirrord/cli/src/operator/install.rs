//! `mirrord operator install`: gets a license, installs the operator into the cluster of the
//! current kubecontext, and hands ownership of the license to the user.

use std::{io::IsTerminal, ops::Not};

use kube::Client;
use mirrord_kube::api::kubernetes::create_kube_config_with_context;
use mirrord_progress::{Progress, ProgressTracker};

use self::{manifest::Manifest, signup::Trial};
use crate::config::OperatorInstallArgs;

mod cluster;
mod error;
mod manifest;
mod signup;

pub(crate) use error::OperatorInstallError;

const USER_AGENT: &str = concat!("mirrord-cli/", env!("CARGO_PKG_VERSION"));

pub(super) async fn operator_install(
    args: OperatorInstallArgs,
) -> Result<(), OperatorInstallError> {
    let OperatorInstallArgs {
        api_key,
        no_browser,
        cluster_hint,
        no_hint,
        manifest: manifest_path,
        app_url,
    } = args;

    let mut progress = ProgressTracker::from_env("mirrord operator install");

    let (mut kube_config, context) = create_kube_config_with_context(None, None::<&str>, None)
        .await
        .map_err(|error| OperatorInstallError::KubeConfig(Box::new(error)))?;
    // The default policy retries 503s for minutes, which is exactly how a registered but
    // unavailable operator API answers, both when checking for an existing operator and while
    // waiting for the new one to come up.
    kube_config.default_retry = false;
    let release_namespace = kube_config.default_namespace.clone();
    let client = Client::try_from(kube_config)
        .map_err(|error| OperatorInstallError::KubeClient(Box::new(error)))?;
    let http = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .build()
        .map_err(OperatorInstallError::HttpClient)?;

    let mut subtask = progress.subtask("checking for an existing operator");
    cluster::ensure_no_operator(&client).await?;
    subtask.success(Some("no operator installed"));

    let mut subtask = progress.subtask("fetching the operator manifest");
    let manifest = match &manifest_path {
        Some(path) => manifest::read_manifest(path)?,
        None => {
            let version = manifest::latest_chart_version(&http).await?;
            manifest::fetch_manifest(&http, version).await?
        }
    };
    let mut manifest = Manifest::parse(&manifest)?;
    manifest.attribute_to_release(&release_namespace);
    subtask.success(None);

    let mut subtask = progress.subtask("checking permissions");
    let apis = cluster::resolve_apis(&client, &manifest, &release_namespace).await?;
    cluster::dry_run(&manifest, &apis).await?;
    subtask.success(None);

    let trial = match api_key {
        Some(api_key) => {
            manifest.set_api_key(&api_key);
            None
        }
        None => {
            let mut subtask = progress.subtask("starting a trial");
            let cluster_hint = no_hint
                .not()
                .then(|| cluster_hint.or(context.clone()))
                .flatten();
            let trial =
                signup::start_trial(&http, &app_url, USER_AGENT, cluster_hint.as_deref()).await?;
            manifest.set_api_key(&trial.api_key);
            subtask.success(None);
            Some(trial)
        }
    };

    let installed = async {
        let mut subtask = progress.subtask("installing the operator");
        cluster::create(&manifest, &apis).await?;
        subtask.success(None);

        let mut subtask = progress.subtask("waiting for the operator to become ready");
        let operator = cluster::wait_for_operator(&client, manifest.operator_namespace()).await?;
        subtask.success(None);

        Ok(operator)
    }
    .await;

    let operator = installed.inspect_err(|_| {
        // The trial exists even though the installation failed. Printed rather than put in the
        // error, where the URL would be wrapped across lines.
        if let Some(trial) = &trial {
            println!("{}", claim_instructions(&trial.claim_url));
        }
    })?;
    progress.success(None);

    Ok(())
}

fn claim_instructions(claim_url: &str) -> String {
    format!("Claim the trial to take ownership of it: {claim_url}")
}

