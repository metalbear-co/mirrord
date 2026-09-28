use futures::TryFutureExt;
use status::StatusCommandHandler;

use self::session::SessionCommandHandler;
use crate::{
    CliResult,
    config::{OperatorArgs, OperatorCommand},
    data::UserData,
    error::{CliError, OperatorSetupError},
};

mod session;
pub(super) mod status;

/// Set up the operator into a file or to stdout, with explanation.
async fn operator_setup() -> CliResult<(), OperatorSetupError> {
    Err(OperatorSetupError::Deleted)
}

/// Handle commands related to the operator `mirrord operator ...`
pub(crate) async fn operator_command(
    args: OperatorArgs,
    watch: drain::Watch,
    user_data: &UserData,
) -> CliResult<()> {
    match args.command {
        OperatorCommand::Setup => operator_setup().await.map_err(CliError::from),
        OperatorCommand::Status { config_file } => {
            StatusCommandHandler::new(config_file, watch, user_data)
                .and_then(StatusCommandHandler::handle)
                .await
        }
        OperatorCommand::Session {
            command,
            config_file,
        } => {
            SessionCommandHandler::new(command, config_file, watch, user_data)
                .and_then(SessionCommandHandler::handle)
                .await
        }
    }
}
