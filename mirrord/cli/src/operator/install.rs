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
            let cluster_hint = match (no_hint, cluster_hint) {
                (true, _) => None,
                (false, Some(cluster_hint)) => Some(cluster_hint),
                (false, None) => cluster::cluster_id(&client).await,
            };
            let trial =
                signup::start_trial(&http, &app_url, USER_AGENT, cluster_hint.as_deref()).await?;
            subtask.success(None);

            // Printed right away, so the claim URL and the API key are not lost if the
            // installation fails or is interrupted, and a retry can reuse the trial.
            println!("{}", trial_details(&trial));
            if no_browser.not()
                && std::io::stdout().is_terminal()
                && let Err(error) = opener::open(&trial.claim_url)
            {
                tracing::debug!(?error, "failed to open the claim URL in the browser");
            }

            manifest.set_api_key(&trial.api_key);
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
        // Printed rather than put in the error, where the command would be wrapped across lines.
        if let Some(trial) = &trial {
            println!(
                "To retry without starting another trial, reuse its API key: mirrord operator \
                install --api-key {}",
                trial.api_key
            );
        }
    })?;
    progress.success(None);

    println!(
        "{}",
        summary(
            &operator.spec.operator_version,
            &manifest,
            context.as_deref()
        )
    );

    Ok(())
}

/// What the user needs to know about a trial, printed once as soon as it starts.
fn trial_details(trial: &Trial) -> String {
    format!(
        "Started a mirrord Enterprise trial. It ends on {}, after which the license drops to the \
        Free tier.\nClaim the trial to take ownership of it: {}\nAPI key of the trial: {}\n",
        trial.trial_ends_at.format("%B %-d, %Y"),
        trial.claim_url,
        trial.api_key,
    )
}

/// What the user needs to know after a successful installation, printed once.
fn summary(version: &semver::Version, manifest: &Manifest, context: Option<&str>) -> String {
    let namespace = manifest.operator_namespace();
    let location = match context {
        Some(context) => format!("namespace `{namespace}` of kubecontext `{context}`"),
        None => format!("namespace `{namespace}`"),
    };
    let mut summary = format!("mirrord operator {version} is installed in {location}.\n\n");

    summary.push_str(&format!(
        "This is a default installation. For anything custom (namespace, tolerations, pull \
        secrets, OIDC, ...), manage it with the helm chart, which takes over this installation \
        and keeps its API key:\n\n  helm repo add metalbear {repo}\n  helm install {release} \
        metalbear/{chart} --version {chart_version} \\\n    --set \
        cloud.apiKey.key=\"{api_key}\"",
        chart_version = manifest.chart_version(),
        api_key = manifest.api_key_lookup(),
        repo = manifest::CHARTS_REPO_URL,
        release = manifest::RELEASE_NAME,
        chart = manifest::CHART_NAME,
    ));

    summary
}
