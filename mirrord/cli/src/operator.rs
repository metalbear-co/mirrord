use std::fs::File;

use futures::TryFutureExt;
use mirrord_operator::setup::{LicenseType, Operator};
use serde::Deserialize;
use status::StatusCommandHandler;
use tokio::fs;

use self::session::SessionCommandHandler;
use crate::{
    CliResult, OperatorSetupParams,
    config::{OperatorArgs, OperatorCommand},
    error::{CliError, OperatorSetupError},
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

/// Set up the operator into a file or to stdout, with explanation.
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
        mysql_branching,
        pg_branching,
    }: OperatorSetupParams,
) -> CliResult<(), OperatorSetupError> {
    Err(OperatorSetupError::Deleted)
}

/// Handle commands related to the operator `mirrord operator ...`
pub(crate) async fn operator_command(args: OperatorArgs) -> CliResult<()> {
    match args.command {
        OperatorCommand::Setup(params) => operator_setup(*params).await.map_err(CliError::from),
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
