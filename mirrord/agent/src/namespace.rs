use std::{fs::File, os::fd::OwnedFd};

use crate::error::Result;
use nix::sched::{setns, CloneFlags};

/// Non exhaustive namespace type enum. Add as needed
#[derive(Debug)]
pub(crate) enum NamespaceType {
    Net,
    Cgroup
}

impl NamespaceType {
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
#[tracing::instrument(level = "trace")]
pub fn set_namespace(pid: u64, namespace_type: NamespaceType) -> Result<()> {
    let fd: OwnedFd = File::open(namespace_type.path_from_pid(pid))?.into();

    setns(fd, namespace_type.into())?;
    Ok(())
}
