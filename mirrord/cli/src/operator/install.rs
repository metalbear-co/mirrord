//! `mirrord operator install`, `signup` and `uninstall`: the operator lifecycle without helm.

use std::{io::IsTerminal, ops::Not};

use inquire::{Confirm, InquireError};
use kube::Client;
use mirrord_analytics::{ReportTarget, Reporter};
use mirrord_kube::api::kubernetes::create_kube_config_with_context;
use mirrord_progress::{Progress, ProgressTracker};
use secrecy::ExposeSecret;
use url::Url;

use self::{
    manifest::Manifest,
    telemetry::{InstallPhase, InstallRun, Outcome},
};
use crate::{config::OperatorInstallArgs, data::StoredLoginToken, login};

mod api_key;
mod cluster;
mod error;
mod manifest;
mod signup;
mod telemetry;
#[cfg(test)]
mod tests;
mod uninstall;

pub(crate) use error::OperatorInstallError;
pub(super) use signup::operator_signup;
pub(crate) use telemetry::OperatorTelemetry;
pub(super) use uninstall::operator_uninstall;

const USER_AGENT: &str = concat!("mirrord-cli/", env!("CARGO_PKG_VERSION"));

pub(super) async fn operator_install(
    args: OperatorInstallArgs,
    telemetry: &OperatorTelemetry,
) -> Result<(), OperatorInstallError> {
    let mut reporter = telemetry.reporter(ReportTarget::OperatorInstall);
    reporter
        .get_mut()
        .add("api_key_given", args.api_key.is_some());

    let mut run = InstallRun {
        phase: InstallPhase::Connect,
        retry: None,
    };
    let result = tokio::select! {
        result = install(args, &mut run) => result,
        // Otherwise, the process ends before the run is reported.
        Ok(()) = tokio::signal::ctrl_c() => Err(OperatorInstallError::Interrupted),
    };

    // Printed rather than put in the error, where the commands would be wrapped across lines.
    if result.is_err()
        && let Some(retry) = &run.retry
    {
        println!("{retry}");
    }

    let analytics = reporter.get_mut();
    analytics.add("api_key_created", run.retry.is_some());
    telemetry::add_outcome(
        analytics,
        result.as_ref().map(|()| Outcome::Success),
        run.phase as u32,
    );

    result
}

async fn install(
    args: OperatorInstallArgs,
    run: &mut InstallRun,
) -> Result<(), OperatorInstallError> {
    let OperatorInstallArgs {
        api_key,
        no_browser: _,
        cluster_hint,
        no_hint,
        context,
        yes,
        manifest: manifest_path,
        app_url,
        ready_timeout,
    } = args;

    let credential = match api_key {
        Some(api_key) => Credential::ApiKey(api_key),
        None => {
            run.phase = InstallPhase::Login;
            let app_url = login::backend_base_url(app_url)
                .map_err(|error| OperatorInstallError::Login(Box::new(error)))?;
            let token = api_key::login_token(&app_url).await?;
            Credential::Login { app_url, token }
        }
    };

    run.phase = InstallPhase::Connect;
    let mut progress = ProgressTracker::from_env("mirrord operator install");
    let Connection {
        client,
        http,
        release_namespace,
        context,
    } = Connection::new(context).await?;
    let context_arg = context_flag("--context", context.as_deref());

    run.phase = InstallPhase::CheckExistingOperator;
    let mut subtask = progress.subtask("checking for an existing operator");
    cluster::ensure_no_operator(&client, &context_arg).await?;
    subtask.success(Some("no operator installed"));

    run.phase = InstallPhase::FetchManifest;
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

    run.phase = InstallPhase::CheckPermissions;
    let mut subtask = progress.subtask("checking permissions");
    let apis = cluster::resolve_apis(&client, &manifest, &release_namespace).await?;
    cluster::dry_run(&manifest, &apis, &context_arg).await?;
    subtask.success(None);

    // Asked after the checks, which change nothing, so that an installation that can't succeed
    // fails without asking first.
    run.phase = InstallPhase::Confirm;
    let question = format!(
        "Install the mirrord operator into {}?",
        location(manifest.operator_namespace(), context.as_deref())
    );
    confirm(&progress, yes, &question)?;

    let created_key;
    let api_key = match &credential {
        Credential::ApiKey(api_key) => api_key.as_str(),
        Credential::Login { app_url, token } => {
            run.phase = InstallPhase::CreateApiKey;
            let mut subtask = progress.subtask(&format!(
                "creating the operator API key of the organization of {}",
                token.email
            ));
            let name = match (cluster_hint, no_hint) {
                (Some(cluster_hint), _) => Some(cluster_hint),
                (None, true) => None,
                (None, false) => cluster::cluster_id(&client).await,
            };
            created_key = api_key::create(&http, app_url, token, name.as_deref()).await?;
            run.retry = Some(format!(
                "The operator API key of your organization was created for this installation. To \
                retry, remove what was installed with `mirrord operator uninstall{context_arg}`, \
                revoke the key at {}, then run this command again.\n",
                api_key::settings_url(app_url)
            ));
            subtask.success(None);
            created_key.expose_secret()
        }
    };
    manifest.set_api_key(api_key);

    run.phase = InstallPhase::CreateObjects;
    let mut subtask = progress.subtask("installing the operator");
    cluster::create(&manifest, &apis).await?;
    subtask.success(None);

    run.phase = InstallPhase::WaitForOperator;
    let mut subtask = progress.subtask("waiting for the operator to become ready");
    let operator = cluster::wait_for_operator(
        &client,
        manifest.operator_namespace(),
        ready_timeout,
        &context_arg,
    )
    .await?;
    subtask.success(None);
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

/// What the operator is registered with.
enum Credential {
    /// An operator API key the user passed.
    ApiKey(String),
    /// The login of the user, used to create the operator API key of their organization in the
    /// app at `app_url`.
    Login {
        app_url: Url,
        token: StoredLoginToken,
    },
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
/// Declining, also by cancelling the prompt with Esc, fails with
/// [`OperatorInstallError::Declined`]. Ctrl+C at the prompt fails with
/// [`OperatorInstallError::Interrupted`], like Ctrl+C at any other moment of the command.
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
        Ok(false) | Err(InquireError::OperationCanceled) => Err(OperatorInstallError::Declined),
        Err(InquireError::OperationInterrupted) => Err(OperatorInstallError::Interrupted),
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
    let context_arg = context_flag("--context", context);

    format!(
        "mirrord operator {version} is installed in {}. To remove it, run `mirrord operator \
        uninstall{context_arg}`.",
        location(manifest.operator_namespace(), context)
    )
}
