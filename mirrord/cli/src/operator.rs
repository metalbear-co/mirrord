use std::{fs::File, path::PathBuf, time::Duration};

use kube::Api;
use mirrord_config::{
    config::{ConfigContext, MirrordConfig},
    LayerFileConfig,
};
use mirrord_kube::{api::kubernetes::create_kube_api, error::KubeApiError};
use mirrord_operator::{
    client::OperatorApiError,
    crd::{LicenseInfoOwned, MirrordOperatorCrd, MirrordOperatorSpec, OPERATOR_STATUS_NAME},
    setup::{LicenseType, Operator, OperatorNamespace, OperatorSetup, SetupError, SetupOptions},
};
use mirrord_progress::{Progress, ProgressTracker};
use prettytable::{row, Table};
use serde::Deserialize;
use tokio::fs;
use tracing::warn;

use crate::{
    config::{OperatorArgs, OperatorCommand},
    error::CliError,
    Result,
};

#[derive(Deserialize)]
struct OperatorVersionResponse {
    operator: String,
}

/// Fetches latest version of mirrord operator from our API
async fn get_last_version() -> Result<String, reqwest::Error> {
    let client = reqwest::Client::builder().build()?;
    let response: OperatorVersionResponse = client
        .get("https://version.mirrord.dev/v1/operator/version")
        .send()
        .await?
        .json()
        .await?;

    Ok(response.operator)
}

// TODO(alex) [high] 2023-11-07: 1st step: Perform client validation and don't even install
// operator if Online license is invalid. Can't trust this, but it's nice to have.
// After implementing this, need a way to disable it so I can test the operator-side validation.
/// Setup the operator into a file or to stdout, with explanation.
///
/// Here we read the license, which is of type [`LicenseType::Online`] if only `license_key` is
/// present, but we don't validate/check it.
async fn operator_setup(
    accept_tos: bool,
    file: Option<PathBuf>,
    namespace: OperatorNamespace,
    license_key: Option<String>,
    license_path: Option<PathBuf>,
) -> Result<()> {
    if !accept_tos {
        eprintln!("Please note that mirrord operator installation requires an active subscription for the mirrord Operator provided by MetalBear Tech LTD.\nThe service ToS can be read here - https://metalbear.co/legal/terms\nPass --accept-tos to accept the TOS");

        return Ok(());
    }

    // TODO(alex) [low] 2023-11-07: Convenience so we can use `SetupError` and not polute
    // `CliError`.
    let get_license = || async move {
        let license = match (license_key, license_path) {
            (_, Some(license_path)) => fs::read_to_string(&license_path)
                .await
                .map(LicenseType::Offline)
                .map_err(From::from),

            (Some(license_key), _) => reqwest::Client::new()
                .get("https://app.metalbear.co/api/v1/license")
                .header("x-license-key", &license_key)
                .send()
                .await?
                .error_for_status()
                .map(|_| LicenseType::Online(license_key))
                .map_err(|fail| {
                    fail.status()
                        .map(|fail_status| {
                            if fail_status.as_u16() == 404 {
                                SetupError::MissingLicense
                            } else {
                                SetupError::from(fail)
                            }
                        })
                        // TODO(alex): Convert this error
                        .unwrap_or_else(|| From::from(fail))
                }),
            (None, None) => Err(SetupError::MissingLicense),
        }?;

        Ok::<_, SetupError>(license)
    };

    let license = get_license().await?;

    // if env var std::env::var("MIRRORD_OPERATOR_IMAGE") exists, use it, otherwise call async
    // function to get it
    let image = match std::env::var("MIRRORD_OPERATOR_IMAGE") {
        Ok(image) => image,
        Err(_) => {
            let version = get_last_version()
                .await
                .map_err(CliError::OperatorVersionCheckError)?;
            format!("ghcr.io/metalbear-co/operator:{version}")
        }
    };

    eprintln!(
        "Installing mirrord operator with namespace: {}",
        namespace.name()
    );

    let operator = Operator::new(SetupOptions {
        license,
        namespace,
        image,
    });

    match file {
        Some(path) => {
            operator.to_writer(File::create(path).map_err(CliError::ManifestFileError)?)?
        }
        None => operator.to_writer(std::io::stdout()).unwrap(), /* unwrap because failing to
                                                                 * write to std out.. well.. */
    }

    Ok(())
}

async fn get_status_api(config: Option<String>) -> Result<Api<MirrordOperatorCrd>> {
    let kube_api = if let Some(config_path) = config {
        let mut cfg_context = ConfigContext::default();
        let config = LayerFileConfig::from_path(config_path)?.generate_config(&mut cfg_context)?;
        create_kube_api(
            config.accept_invalid_certificates,
            config.kubeconfig,
            config.kube_context,
        )
    } else {
        create_kube_api(false, None, None)
    }
    .await
    .map_err(CliError::KubernetesApiFailed)?;

    Ok(Api::all(kube_api))
}

async fn operator_status(config: Option<String>) -> Result<()> {
    let mut progress = ProgressTracker::from_env("Operator Status");

    let status_api = get_status_api(config).await?;

    let mut status_progress = progress.subtask("fetching status");

    let mirrord_status = match status_api
        .get(OPERATOR_STATUS_NAME)
        .await
        .inspect_err(|fail| {
            tracing::error!("\nstatus.get error {fail:?}\n");
            match fail {
                kube::Error::SerdeError(fs) => match fs.classify() {
                    // TODO(alex): It's this type, but we should report this as a missing
                    // license / invalid license.
                    serde_json::error::Category::Data => tracing::error!("\ndata error {fs:#?}\n"),
                    _ => tracing::error!("\nserde error is {fs:#?}\n"),
                },
                _ => tracing::error!("\nother type of fail {fail:#?}\n"),
            }
        })
        .map_err(KubeApiError::KubeError)
        .map_err(OperatorApiError::KubeApiError)
        .map_err(CliError::OperatorConnectionFailed)
    {
        Ok(status) => status,
        Err(err) => {
            status_progress.failure(Some("unable to get status"));

            return Err(err);
        }
    };

    status_progress.success(Some("fetched status"));

    progress.success(None);

    let MirrordOperatorSpec {
        operator_version,
        default_namespace,
        license:
            LicenseInfoOwned {
                name,
                organization,
                expire_at,
                ..
            },
        ..
    } = mirrord_status.spec;

    let expire_at = expire_at.format("%e-%b-%Y");

    println!(
        r#"
Operator version: {operator_version}
Operator default namespace: {default_namespace}
Operator License
    name: {name}
    organization: {organization}
    expire at: {expire_at}
"#
    );

    let Some(status) = mirrord_status.status else {
        return Ok(());
    };

    if let Some(statistics) = status.statistics {
        println!("Operator Daily Users: {}", statistics.dau);
        println!("Operator Monthly Users: {}", statistics.mau);
    }

    let mut sessions = Table::new();

    sessions.add_row(row!["Session ID", "Target", "User", "Session Duration"]);

    for session in &status.sessions {
        sessions.add_row(row![
            session.id.as_deref().unwrap_or(""),
            &session.target,
            &session.user,
            humantime::format_duration(Duration::from_secs(session.duration_secs)),
        ]);
    }

    sessions.printstd();

    Ok(())
}

/// Handle commands related to the operator `mirrord operator ...`
pub(crate) async fn operator_command(args: OperatorArgs) -> Result<()> {
    match args.command {
        OperatorCommand::Setup {
            accept_tos,
            file,
            namespace,
            license_key,
            license_path,
        } => operator_setup(accept_tos, file, namespace, license_key, license_path).await,
        OperatorCommand::Status { config_file } => operator_status(config_file).await,
    }
}
