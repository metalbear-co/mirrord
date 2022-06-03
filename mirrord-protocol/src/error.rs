use std::io;

use bincode::{Decode, Encode};
use thiserror::Error;

#[derive(Encode, Decode, Debug, PartialEq, Clone, Eq, Error)]
pub enum ResponseError {
    #[error("Response for `FileOperation` failed with `{0}!")]
    FileOperation(FileError),

    #[error("Failed to find resource!")]
    NotFound,
}

#[derive(Encode, Decode, Debug, PartialEq, Clone, Eq, Error)]
#[error("Failed performing file operation {operation:?} with {raw_os_error:?} and kind {kind:?}!")]
pub struct FileError {
    pub operation: String,
    pub raw_os_error: Option<i32>,
    pub kind: ErrorKindInternal,
}

/// Alternative to `std::io::ErrorKind`, used to implement `bincode::Encode` and `bincode::Decode`.
#[derive(Encode, Decode, Debug, PartialEq, Clone, Copy, Eq)]
pub enum ErrorKindInternal {
    NotFound,
    PermissionDenied,
    ConnectionRefused,
    ConnectionReset,
    HostUnreachable,
    NetworkUnreachable,
    ConnectionAborted,
    NotConnected,
    AddrInUse,
    AddrNotAvailable,
    NetworkDown,
    BrokenPipe,
    AlreadyExists,
    WouldBlock,
    NotADirectory,
    IsADirectory,
    DirectoryNotEmpty,
    ReadOnlyFilesystem,
    FilesystemLoop,
    StaleNetworkFileHandle,
    InvalidInput,
    InvalidData,
    TimedOut,
    WriteZero,
    StorageFull,
    NotSeekable,
    FilesystemQuotaExceeded,
    FileTooLarge,
    ResourceBusy,
    ExecutableFileBusy,
    Deadlock,
    CrossesDevices,
    TooManyLinks,
    InvalidFilename,
    ArgumentListTooLong,
    Interrupted,
    Unsupported,
    UnexpectedEof,
    OutOfMemory,
    Other,
}

impl const From<io::ErrorKind> for ErrorKindInternal {
    fn from(error_kind: io::ErrorKind) -> Self {
        match error_kind {
            io::ErrorKind::NotFound => ErrorKindInternal::NotFound,
            io::ErrorKind::PermissionDenied => ErrorKindInternal::PermissionDenied,
            io::ErrorKind::ConnectionRefused => ErrorKindInternal::ConnectionRefused,
            io::ErrorKind::ConnectionReset => ErrorKindInternal::ConnectionReset,
            io::ErrorKind::HostUnreachable => ErrorKindInternal::HostUnreachable,
            io::ErrorKind::NetworkUnreachable => ErrorKindInternal::NetworkUnreachable,
            io::ErrorKind::ConnectionAborted => ErrorKindInternal::ConnectionAborted,
            io::ErrorKind::NotConnected => ErrorKindInternal::NotConnected,
            io::ErrorKind::AddrInUse => ErrorKindInternal::AddrInUse,
            io::ErrorKind::AddrNotAvailable => ErrorKindInternal::AddrNotAvailable,
            io::ErrorKind::NetworkDown => ErrorKindInternal::NetworkDown,
            io::ErrorKind::BrokenPipe => ErrorKindInternal::BrokenPipe,
            io::ErrorKind::AlreadyExists => ErrorKindInternal::AlreadyExists,
            io::ErrorKind::WouldBlock => ErrorKindInternal::WouldBlock,
            io::ErrorKind::NotADirectory => ErrorKindInternal::NotADirectory,
            io::ErrorKind::IsADirectory => ErrorKindInternal::IsADirectory,
            io::ErrorKind::DirectoryNotEmpty => ErrorKindInternal::DirectoryNotEmpty,
            io::ErrorKind::ReadOnlyFilesystem => ErrorKindInternal::ReadOnlyFilesystem,
            io::ErrorKind::FilesystemLoop => ErrorKindInternal::FilesystemLoop,
            io::ErrorKind::StaleNetworkFileHandle => ErrorKindInternal::StaleNetworkFileHandle,
            io::ErrorKind::InvalidInput => ErrorKindInternal::InvalidInput,
            io::ErrorKind::InvalidData => ErrorKindInternal::InvalidData,
            io::ErrorKind::TimedOut => ErrorKindInternal::TimedOut,
            io::ErrorKind::WriteZero => ErrorKindInternal::WriteZero,
            io::ErrorKind::StorageFull => ErrorKindInternal::StorageFull,
            io::ErrorKind::NotSeekable => ErrorKindInternal::NotSeekable,
            io::ErrorKind::FilesystemQuotaExceeded => ErrorKindInternal::FilesystemQuotaExceeded,
            io::ErrorKind::FileTooLarge => ErrorKindInternal::FileTooLarge,
            io::ErrorKind::ResourceBusy => ErrorKindInternal::ResourceBusy,
            io::ErrorKind::ExecutableFileBusy => ErrorKindInternal::ExecutableFileBusy,
            io::ErrorKind::Deadlock => ErrorKindInternal::Deadlock,
            io::ErrorKind::CrossesDevices => ErrorKindInternal::CrossesDevices,
            io::ErrorKind::TooManyLinks => ErrorKindInternal::TooManyLinks,
            io::ErrorKind::InvalidFilename => ErrorKindInternal::InvalidFilename,
            io::ErrorKind::ArgumentListTooLong => ErrorKindInternal::ArgumentListTooLong,
            io::ErrorKind::Interrupted => ErrorKindInternal::Interrupted,
            io::ErrorKind::Unsupported => ErrorKindInternal::Unsupported,
            io::ErrorKind::UnexpectedEof => ErrorKindInternal::UnexpectedEof,
            io::ErrorKind::OutOfMemory => ErrorKindInternal::OutOfMemory,
            io::ErrorKind::Other => ErrorKindInternal::Other,
            _ => unimplemented!(),
        }
    }
}
