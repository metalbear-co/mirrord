use thiserror::Error;

pub type Result<T> = std::result::Result<T, SipError>;

#[derive(Debug, Error)]
pub enum SipError {
    #[error("IO failed with `{0}`")]
    IO(#[from] std::io::Error),

    #[error("Signing failed statuscode: `{0}`, output: `{1}`")]
    Sign(i32, String),

    #[error("Adding Rpaths failed with statuscode: `{0}`, output: `{1}`")]
    AddingRpathsFailed(i32, String),

    #[error("Can't patch file format `{0}`")]
    UnsupportedFileFormat(String),

    #[error(
        "No supported architecture in file (x86_64 on intel chips, x86_64 or arm64 on apple chips)"
    )]
    NoSupportedArchitecture,

    #[error("ObjectParse failed with `{0}`")]
    ObjectParse(#[from] object::Error),

    #[error("which failed with `{0}`")]
    WhichFailed(#[from] which::Error),

    #[error("Unlikely error happened `{0}`")]
    UnlikelyError(String),

    #[error("Can't perform SIP check - executable file not found at `{0}`")]
    FileNotFound(String),

    #[error("Got invalid string.")]
    NonUtf8Str(#[from] std::str::Utf8Error),
}
