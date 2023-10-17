use std::io;

use miette::Diagnostic;
use mirrord_intproxy::codec::CodecError;
use thiserror::Error;

pub type Result<T, E = ConsoleError> = std::result::Result<T, E>;

#[derive(Error, Diagnostic, Debug)]
pub enum ConsoleError {
    #[error("failed to connect to console `{0}`")]
    #[diagnostic(help("Please check that the console is running and address is correct."))]
    Connect(#[from] io::Error),
    #[error("an error occured after connection `{0}`")]
    #[diagnostic(help("Please report a bug."))]
    Codec(#[from] CodecError),
    #[error("setting logger failed `{0}`")]
    #[diagnostic(help("Please report a bug."))]
    SetLogger(#[from] log::SetLoggerError),
}
