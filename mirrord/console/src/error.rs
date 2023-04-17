use miette::Diagnostic;
use thiserror::Error;

pub type Result<T, E = ConsoleError> = std::result::Result<T, E>;

#[derive(Error, Diagnostic, Debug)]
pub enum ConsoleError {
    #[error("failed to connect to console `{0}`")]
    #[diagnostic(help("Please check that the console is running and address is correct."))]
    ConnectError(#[source] tungstenite::Error),
    #[error("WS Socket occured after connection `{0}`")]
    #[diagnostic(help("Please report a bug."))]
    WsSocketError(#[from] tungstenite::Error),
    #[error("Set logger failed `{0}`")]
    #[diagnostic(help("Please report a bug."))]
    SetLoggerError(#[from] log::SetLoggerError),
}
