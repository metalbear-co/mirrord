use std::{fs::File, path::PathBuf, time::Duration};

use kube::{api::DeleteParams, Api};
use mirrord_config::{
    config::{ConfigContext, MirrordConfig},
    LayerFileConfig,
};
use mirrord_kube::api::kubernetes::create_kube_api;
use mirrord_operator::{
    client::{session_api, OperatorApiError, OperatorOperation},
    crd::{LicenseInfoOwned, MirrordOperatorCrd, MirrordOperatorSpec, OPERATOR_STATUS_NAME},
    setup::{LicenseType, Operator, OperatorNamespace, OperatorSetup, SetupOptions},
};
use mirrord_progress::{Progress, ProgressTracker};
use prettytable::{row, Table};
use serde::Deserialize;
use tokio::fs;
use tracing::{error, warn};

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

/// Setup the operator into a file or to stdout, with explanation.
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

    let license = match (license_key, license_path) {
        (_, Some(license_path)) => fs::read_to_string(&license_path)
            .await
            .inspect_err(|err| {
                warn!(
                    "Unable to read license at path {}: {err}",
                    license_path.display()
                )
            })
            .ok()
            .map(LicenseType::Offline),
        (Some(license_key), _) => Some(LicenseType::Online(license_key)),
        (None, None) => None,
    };

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

    if let Some(license) = license {
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
            None => operator.to_writer(std::io::stdout()).unwrap(), /* unwrap because failing to write to std out.. well.. */
        }
    } else {
        eprintln!("--license-key or --license-path is required to install on cluster");
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

#[tracing::instrument(level = "debug", ret)]
async fn operator_status(config: Option<String>) -> Result<()> {
    let mut progress = ProgressTracker::from_env("Operator Status");

    tracing::info!("GET_STATUS_API");
    let status_api = get_status_api(config).await?;
    tracing::info!("HAVE STATUS API");

    let mut status_progress = progress.subtask("fetching status");

    let mirrord_status = match status_api
        .get(OPERATOR_STATUS_NAME)
        .await
        .map_err(|error| OperatorApiError::KubeError {
            error,
            operation: OperatorOperation::GettingStatus,
        })
        .map_err(CliError::from)
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

    if let Some(copy_targets) = status.copy_targets {
        if copy_targets.is_empty() {
            println!("No active copy targets.");
        } else {
            println!("Active Copy Targets:");
            let mut copy_targets_table = Table::new();

            copy_targets_table.add_row(row![
                "Original Target",
                "Namespace",
                "Copy Pod Name",
                "Scale Down?"
            ]);

            for (pod_name, copy_target_resource) in copy_targets {
                copy_targets_table.add_row(row![
                    copy_target_resource.spec.target.to_string(),
                    copy_target_resource.metadata.namespace.unwrap_or_default(),
                    pod_name,
                    if copy_target_resource.spec.scale_down {
                        "*"
                    } else {
                        ""
                    },
                ]);
            }

            copy_targets_table.printstd();
        }
        println!();
    }

    if let Some(statistics) = status.statistics {
        println!("Operator Daily Users: {}", statistics.dau);
        println!("Operator Monthly Users: {}", statistics.mau);
    }

    let mut sessions = Table::new();

    sessions.add_row(row![
        "Session ID",
        "Target",
        "Namespace",
        "User",
        "Ports",
        "Session Duration"
    ]);

    for session in &status.sessions {
        let locked_ports = session
            .locked_ports
            .as_deref()
            .map(|ports| {
                ports
                    .iter()
                    .map(|(port, type_, filter)| {
                        format!(
                            "Port: {port}, Type: {type_}{}",
                            filter
                                .as_ref()
                                .map(|f| format!(", Filter: {}", f))
                                .unwrap_or_default()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();

        sessions.add_row(row![
            session.id.as_deref().unwrap_or(""),
            &session.target,
            session.namespace.as_deref().unwrap_or("N/A"),
            &session.user,
            locked_ports,
            humantime::format_duration(Duration::from_secs(session.duration_secs)),
        ]);
    }

    sessions.printstd();

    Ok(())
}

/// Handles the `mirrord operator session` family of commands:
/// - `--kill-one {session_id}`: kills the operator session specified by `session_id`;
/// - `--kill-all`: kills every operator session, this is basically a `.clear()`;
/// - `--retain-active`: performs a clean-up for operator sessions that are still stored;
#[tracing::instrument(level = "debug", ret)]
async fn operator_session(
    kill_one: Option<u32>,
    kill_all: bool,
    retain_active: bool,
) -> Result<()> {
    let mut progress = ProgressTracker::from_env("Operator session action");

    let session_api = session_api(None)
        .await
        .inspect_err(|fail| error!("Failed to even get sessio_api {fail:?}!"))?;

    let mut operation_progress = progress.subtask("preparing to execute session operation...");

    let result = if let Some(session_id) = kill_one {
        operation_progress.print("killing session with id {session_id}");

        session_api
            .delete(&format!("kill_one/{session_id}"), &DeleteParams::default())
            .await
            .map_err(|error| OperatorApiError::KubeError {
                error,
                operation: OperatorOperation::GettingStatus,
            })
            .map_err(CliError::from)
    } else if kill_all {
        operation_progress.print("killing all sessions");

        session_api
            .delete(&format!("kill_all"), &DeleteParams::default())
            .await
            .map_err(|error| OperatorApiError::KubeError {
                error,
                operation: OperatorOperation::GettingStatus,
            })
            .map_err(CliError::from)
    } else if retain_active {
        operation_progress.print("retaining only active sessions");

        session_api
            .delete(&format!("retain_active"), &DeleteParams::default())
            .await
            .map_err(|error| OperatorApiError::KubeError {
                error,
                operation: OperatorOperation::GettingStatus,
            })
            .map_err(CliError::from)
    } else {
        panic!("You must select one of the session options, there is no default!");
    }
    .inspect_err(|_| {
        operation_progress.failure(Some("Failed to execute session operation!"));
    })?;

    if let Some(status) = result.right() {
        operation_progress.success(Some(&format!(
            "session operation completed with {status:?}"
        )));
    }

    operation_progress.success(Some("session operation finished"));
    progress.success(Some("Done with session stuff!"));

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
        OperatorCommand::Session {
            kill,
            kill_all,
            retain_active,
        } => operator_session(kill, kill_all, retain_active).await,
    }
}
