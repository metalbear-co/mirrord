//! `mirrord operator install`: gets a license, installs the operator into the cluster of a
//! kubecontext, and hands ownership of the license to the user. `mirrord operator uninstall`
//! removes it again, see [`uninstall`].

use std::{io::IsTerminal, ops::Not};

use inquire::{Confirm, InquireError};
use kube::Client;
use mirrord_kube::api::kubernetes::create_kube_config_with_context;
use mirrord_progress::{Progress, ProgressTracker};

use self::{manifest::Manifest, signup::Trial};
use crate::config::OperatorInstallArgs;

mod cluster;
mod error;
mod manifest;
mod signup;
#[cfg(test)]
mod tests;
mod uninstall;

pub(crate) use error::OperatorInstallError;
pub(super) use uninstall::operator_uninstall;

const USER_AGENT: &str = concat!("mirrord-cli/", env!("CARGO_PKG_VERSION"));

pub(super) async fn operator_install(
    args: OperatorInstallArgs,
) -> Result<(), OperatorInstallError> {
    let OperatorInstallArgs {
        api_key,
        no_browser,
        cluster_hint,
        no_hint,
        context,
        yes,
        manifest: manifest_path,
        app_url,
    } = args;

    let mut progress = ProgressTracker::from_env("mirrord operator install");
    let Connection {
        client,
        http,
        release_namespace,
        context,
    } = Connection::new(context).await?;
    let context_arg = context_flag("--context", context.as_deref());

    let mut subtask = progress.subtask("checking for an existing operator");
    cluster::ensure_no_operator(&client, &context_arg).await?;
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
    cluster::dry_run(&manifest, &apis, &context_arg).await?;
    subtask.success(None);

    // Asked after the checks, which change nothing, so that an installation that can't succeed
    // fails without asking first.
    let question = format!(
        "Install the mirrord operator into {}?",
        location(manifest.operator_namespace(), context.as_deref())
    );
    confirm(&progress, yes, &question)?;

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
        let operator =
            cluster::wait_for_operator(&client, manifest.operator_namespace(), &context_arg)
                .await?;
        subtask.success(None);

        Ok(operator)
    }
    .await;

    let operator = installed.inspect_err(|_| {
        // Printed rather than put in the error, where the commands would be wrapped across lines.
        if let Some(trial) = &trial {
            println!(
                "To retry without starting another trial, remove what was installed, then reuse \
                the API key of the trial:\n\n  mirrord operator uninstall{context_arg}\n  mirrord \
                operator install{context_arg} --api-key {}\n",
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

/// What the operator commands need to reach the cluster and the internet.
struct Connection {
    client: Client,
    http: reqwest::Client,
    /// The default namespace of the kubecontext, where `helm install` puts its release.
    release_namespace: String,
    /// The name of the kubecontext, if it has one.
    context: Option<String>,
}

impl Connection {
    /// Loads the given kubecontext, or the current one, and creates the clients.
    async fn new(context: Option<String>) -> Result<Self, OperatorInstallError> {
        let (mut kube_config, context) =
            create_kube_config_with_context(None, None::<&str>, context)
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

        Ok(Self {
            client,
            http,
            release_namespace,
            context,
        })
    }
}

/// Asks the user to confirm a change to the cluster, so that a wrong current kubecontext does not
/// get changed by mistake.
///
/// Does not ask with `yes`, or without a terminal, so that agents and CI can run the command.
/// Declining, also by cancelling the prompt (e.g. with Ctrl+C), fails with
/// [`OperatorInstallError::Declined`].
fn confirm(
    progress: &ProgressTracker,
    yes: bool,
    question: &str,
) -> Result<(), OperatorInstallError> {
    if yes || std::io::stdin().is_terminal().not() {
        return Ok(());
    }

    // Blocks the runtime until the user answers. No other task has work to do meanwhile, and the
    // clients connect again if the server closed an idle connection.
    match progress.suspend(|| Confirm::new(question).with_default(false).prompt()) {
        Ok(true) => Ok(()),
        Ok(false) | Err(InquireError::OperationCanceled | InquireError::OperationInterrupted) => {
            Err(OperatorInstallError::Declined)
        }
        Err(error) => Err(OperatorInstallError::Prompt(error)),
    }
}

/// Names the namespace and the kubecontext the operator is in, for messages to the user.
fn location(namespace: &str, context: Option<&str>) -> String {
    match context {
        Some(context) => format!("namespace `{namespace}` of kubecontext `{context}`"),
        None => format!("namespace `{namespace}`"),
    }
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

/// The `flag` that gives a command printed to the user the kubecontext of the run, so that the
/// command does not use a different cluster if the kubecontext is not the current one. Empty if
/// the kubecontext has no name.
///
/// The name is quoted for the shell, since a kubeconfig can give a kubecontext any name, also
/// with spaces or shell syntax.
fn context_flag(flag: &str, context: Option<&str>) -> String {
    context
        .map(|context| {
            let context = shlex::try_quote(context).unwrap_or(context.into());
            format!(" {flag} {context}")
        })
        .unwrap_or_default()
}

/// What the user needs to know after a successful installation, printed once.
fn summary(version: &semver::Version, manifest: &Manifest, context: Option<&str>) -> String {
    let kube_context_arg = context_flag("--kube-context", context);
    let context_arg = context_flag("--context", context);

    let mut summary = format!(
        "mirrord operator {version} is installed in {}. To remove it, run `mirrord operator \
        uninstall{context_arg}`.\n\n",
        location(manifest.operator_namespace(), context)
    );
    summary.push_str(&format!(
        "This is a default installation. For anything custom (namespace, tolerations, pull \
        secrets, OIDC, ...), manage it with the helm chart, which takes over this installation \
        and keeps its API key:\n\n  helm repo add metalbear {repo}\n  helm install {release} \
        metalbear/{chart} --version {chart_version}{kube_context_arg} \\\n    --set \
        cloud.apiKey.key=\"{api_key}\"",
        chart_version = manifest.chart_version(),
        api_key = manifest.api_key_lookup(&context_arg),
        repo = manifest::CHARTS_REPO_URL,
        release = manifest::RELEASE_NAME,
        chart = manifest::CHART_NAME,
    ));

    summary
}
