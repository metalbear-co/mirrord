use std::{fs::File, os::fd::AsRawFd};

use nix::sched::{setns, CloneFlags};
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum NamespaceError {
    #[error("Failed opening pid's namespace file: {0}")]
    FailedNamespaceOpen(#[from] std::io::Error),
    #[error("Failed to enter namespace: {0}")]
    FailedNamespaceEnter(#[from] nix::Error),
}

/// Non exhaustive namespace type enum. Add as needed
#[derive(Debug)]
pub(crate) enum NamespaceType {
    Net,
    Cgroup,
}

impl NamespaceType {
    #[tracing::instrument(level = "trace", ret)]
    fn path_from_pid(&self, pid: u64) -> String {
        match self {
            NamespaceType::Net => format!("/proc/{}/ns/net", pid),
            NamespaceType::Cgroup => format!("/proc/{}/ns/cgroup", pid),
        }
    }
}

impl From<NamespaceType> for CloneFlags {
    fn from(ns_type: NamespaceType) -> Self {
        match ns_type {
            NamespaceType::Net => CloneFlags::CLONE_NEWNET,
            NamespaceType::Cgroup => CloneFlags::CLONE_NEWCGROUP,
        }
    }
}

/// Set namespace by cloneflags and pid.
/// NOTE: don't make it async in the case we're in an multi-thread scheduler and we want it to
/// happen on the same thread always.
#[tracing::instrument(level = "trace")]
pub(crate) fn set_namespace(pid: u64, namespace_type: NamespaceType) -> Result<(), NamespaceError> {
    let fd = File::open(namespace_type.path_from_pid(pid))?;

    // use as_raw_fd to get reference so it will drop after setns
    setns(fd.as_raw_fd(), namespace_type.into())?;
    Ok(())
}
