use std::{
    fs::File,
    path::{Path, PathBuf},
    time::Duration,
};

use futures::TryFutureExt;
use kube::{Api, Client};
use mirrord_analytics::NullReporter;
use mirrord_config::{
    config::{ConfigContext, MirrordConfig},
    LayerConfig, LayerFileConfig,
};
use mirrord_kube::api::kubernetes::create_kube_config;
use mirrord_operator::{
    client::OperatorApi,
    crd::{MirrordOperatorCrd, MirrordOperatorSpec},
    setup::{LicenseType, Operator, OperatorNamespace, OperatorSetup, SetupOptions},
    types::LicenseInfoOwned,
};
use mirrord_progress::{Progress, ProgressTracker};
use prettytable::{row, Table};
use serde::Deserialize;
use tokio::fs;
use tracing::{warn, Level};

use self::session::SessionCommandHandler;
use crate::{
    config::{OperatorArgs, OperatorCommand},
    error::{CliError, OperatorSetupError},
    util::remove_proxy_env,
    Result,
};

mod session;

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
) -> Result<(), OperatorSetupError> {
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
            let version = get_last_version().await?;
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
                let writer =
                    File::create(&path).map_err(|e| OperatorSetupError::OutputFileOpen(path, e))?;
                operator.to_writer(writer)?;
            }
            None => operator.to_writer(std::io::stdout()).unwrap(), /* unwrap because failing to write to std out.. well.. */
        }
    } else {
        eprintln!("--license-key or --license-path is required to install on cluster");
    }

    Ok(())
}

#[tracing::instrument(level = Level::TRACE, ret)]
async fn get_status_api(config: Option<&Path>) -> Result<Api<MirrordOperatorCrd>> {
    let layer_config = if let Some(config) = config {
        let mut cfg_context = ConfigContext::default();
        LayerFileConfig::from_path(config)?.generate_config(&mut cfg_context)?
    } else {
        LayerConfig::from_env()?
    };

    if !layer_config.use_proxy {
        remove_proxy_env();
    }

    let client = create_kube_config(
        layer_config.accept_invalid_certificates,
        layer_config.kubeconfig,
        layer_config.kube_context,
    )
    .await
    .and_then(|config| Client::try_from(config).map_err(From::from))
    .map_err(CliError::CreateKubeApiFailed)?;

    Ok(Api::all(client))
}

#[tracing::instrument(level = Level::TRACE, ret)]
async fn operator_status(config: Option<&Path>) -> Result<()> {
    let mut progress = ProgressTracker::from_env("Operator Status");

    let layer_config = if let Some(config) = config {
        let mut cfg_context = ConfigContext::default();
        LayerFileConfig::from_path(config)?.generate_config(&mut cfg_context)?
    } else {
        LayerConfig::from_env()?
    };

    if !layer_config.use_proxy {
        remove_proxy_env();
    }

    let mut status_progress = progress.subtask("fetching status");
    let api = OperatorApi::try_new(&layer_config, &mut NullReporter::default())
        .await
        .inspect_err(|_| {
            status_progress.failure(Some("failed to get status"));
        })?;
    let Some(api) = api else {
        status_progress.failure(Some("operator not found"));
        return Err(CliError::OperatorNotInstalled);
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
    } = &api.operator().spec;

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

    let Some(status) = &api.operator().status else {
        return Ok(());
    };

    if let Some(copy_targets) = status.copy_targets.as_ref() {
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
                    copy_target_resource
                        .metadata
                        .namespace
                        .as_deref()
                        .unwrap_or_default(),
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

    if let Some(statistics) = status.statistics.as_ref() {
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

/// Handle commands related to the operator `mirrord operator ...`
pub(crate) async fn operator_command(args: OperatorArgs) -> Result<()> {
    match args.command {
        OperatorCommand::Setup {
            accept_tos,
            file,
            namespace,
            license_key,
            license_path,
        } => operator_setup(accept_tos, file, namespace, license_key, license_path)
            .await
            .map_err(CliError::from),
        OperatorCommand::Status { config_file } => operator_status(config_file.as_deref()).await,
        OperatorCommand::Session(session_command) => {
            SessionCommandHandler::new(session_command)
                .and_then(SessionCommandHandler::handle)
                .await
        }
    }
}
