use std::fs::File;

use futures::TryFutureExt;
use mirrord_operator::setup::{LicenseType, Operator, OperatorSetup, SetupOptions};
use serde::Deserialize;
use status::StatusCommandHandler;
use tokio::fs;

use self::session::SessionCommandHandler;
use crate::{
    config::{OperatorArgs, OperatorCommand},
    error::{CliError, OperatorSetupError},
    CliResult, OperatorSetupParams,
};

mod session;
pub(super) mod status;

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
        application_auto_pause,
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
                tracing::warn!(
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
            application_auto_pause,
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

/// Handle commands related to the operator `mirrord operator ...`
pub(crate) async fn operator_command(args: OperatorArgs) -> CliResult<()> {
    match args.command {
        OperatorCommand::Setup(params) => operator_setup(params).await.map_err(CliError::from),
        OperatorCommand::Status { config_file } => {
            StatusCommandHandler::new(config_file)
                .and_then(StatusCommandHandler::handle)
                .await
        }
        OperatorCommand::Session {
            command,
            config_file,
        } => {
            SessionCommandHandler::new(command, config_file)
                .and_then(SessionCommandHandler::handle)
                .await
        }
    }
}
