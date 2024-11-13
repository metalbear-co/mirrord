use std::{fs::File, path::Path};

use futures::TryFutureExt;
use kube::{Api, Client};
use mirrord_config::{
    config::{ConfigContext, MirrordConfig},
    LayerConfig, LayerFileConfig,
};
use mirrord_kube::api::kubernetes::create_kube_config;
use mirrord_operator::{
    crd::MirrordOperatorCrd,
    setup::{LicenseType, Operator, OperatorSetup, SetupOptions},
};
use serde::Deserialize;
use status::StatusCommandHandler;
use tokio::fs;
use tracing::{warn, Level};

use self::session::SessionCommandHandler;
use crate::{
    config::{OperatorArgs, OperatorCommand},
    error::{CliError, OperatorSetupError},
    util::remove_proxy_env,
    CliResult, OperatorSetupParams,
};

mod session;
mod status;

#[derive(Deserialize)]
struct OperatorVersionResponse {
    operator: String,
}

/// Fetches latest version of mirrord operator from our API
async fn get_last_version() -> CliResult<String, reqwest::Error> {
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
    OperatorSetupParams {
        accept_tos,
        license_key,
        license_path,
        file,
        namespace,
        aws_role_arn,
        sqs_splitting,
        kafka_splitting,
    }: OperatorSetupParams,
) -> CliResult<(), OperatorSetupError> {
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
            aws_role_arn,
            sqs_splitting,
            kafka_splitting,
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
async fn get_status_api(config: Option<&Path>) -> CliResult<Api<MirrordOperatorCrd>> {
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
    .map_err(|error| CliError::friendlier_error_or_else(error, CliError::CreateKubeApiFailed))?;

    Ok(Api::all(client))
}

/// Handle commands related to the operator `mirrord operator ...`
pub(crate) async fn operator_command(args: OperatorArgs) -> CliResult<()> {
    match args.command {
        OperatorCommand::Setup(params) => operator_setup(params).await.map_err(CliError::from),
        OperatorCommand::Status { config_file } => {
            StatusCommandHandler::new(config_file)
                .and_then(StatusCommandHandler::handle)
                .await
        }
        OperatorCommand::Session(session_command) => {
            SessionCommandHandler::new(session_command)
                .and_then(SessionCommandHandler::handle)
                .await
        }
    }
}
