use std::{fmt, fs::File};

use nix::sched::{setns, CloneFlags};
use thiserror::Error;
use tracing::Level;

/// Errors that can occur when entering a Linux namespace.
#[derive(Debug, Error)]
pub enum NamespaceError {
    #[error("failed to open target's namespace file: {0}")]
    FailedNamespaceOpen(#[from] std::io::Error),
    #[error("failed to enter target's namespace: {0}")]
    FailedNamespaceEnter(#[from] nix::Error),
}

/// Linux namespace types.
///
/// Add more as needed.
#[derive(Debug, Clone, Copy)]
pub enum NamespaceType {
    Net,
}

impl NamespaceType {
    /// Returns a path to the namespace file for the given target process ID.
    ///
    /// This path can be used with [`setns`] to enter the namespace.
    fn path_for_target(self, target_pid: u64) -> String {
        match self {
            NamespaceType::Net => format!("/proc/{target_pid}/ns/net"),
        }
    }
}

impl fmt::Display for NamespaceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Net => f.write_str("net"),
        }
    }
}

impl From<NamespaceType> for CloneFlags {
    fn from(ns_type: NamespaceType) -> Self {
        match ns_type {
            NamespaceType::Net => CloneFlags::CLONE_NEWNET,
        }
    }
}

/// Reassociates the current thread with the target's namespace.
#[tracing::instrument(level = Level::TRACE, ret, err)]
pub fn set_namespace(target_pid: u64, namespace_type: NamespaceType) -> Result<(), NamespaceError> {
    let file = File::open(namespace_type.path_for_target(target_pid))?;
    setns(file, namespace_type.into())?;

    Ok(())
}
