//! Logic for pausing using cgroup directly
//! Pause requires privileged - assumes ephemeral (hardcoded pid 1)
use std::{fs::OpenOptions, io::Write, path::Path};

use const_format::formatcp;
use enum_dispatch::enum_dispatch;
use nix::mount::{mount, MsFlags};
use tracing::trace;

use crate::{
    error::Result,
    namespace::{set_namespace, NamespaceType},
};

const CGROUP_MOUNT_PATH: &str = "/mirrord_cgroup";

/// Trait for objects that can be paused
#[enum_dispatch]
pub(crate) trait CgroupFreeze {
    fn pause(&self) -> Result<()> {
        // Enter the namespace, we might be already in it but it doesn't really matter
        // Note: Entering the cgroup namespace **doesn't** set put our process in the cgroup :phew:
        set_namespace(1, NamespaceType::Cgroup)?;
        // Check if our cgroup is mounted, if not, mount it.
        let cgroup_path = Path::new(CGROUP_MOUNT_PATH);
        if !cgroup_path.exists() {
            trace!("mounting cgroup");
            mount(
                None::<&str>,
                cgroup_path,
                Some(self.type_name()),
                MsFlags::empty(),
                None::<&str>,
            )?;
        }
        let mut open_options = OpenOptions::new();
        let mut file = open_options.write(true).open(self.freeze_path())?;
        file.write_all(self.freeze_command().as_bytes())?;
        Ok(())
    }

    fn unpause(&self) -> Result<()> {
        // if we're unpausing, mount should exist and we should be in the cgroup namespace
        let mut open_options = OpenOptions::new();
        let mut file = open_options.write(true).open(self.freeze_path())?;
        file.write_all(self.freeze_command().as_bytes())?;
        Ok(())
    }

    fn type_name(&self) -> &'static str;
    fn freeze_path(&self) -> &'static str;
    fn freeze_command(&self) -> &'static str;
    fn unfreeze_command(&self) -> &'static str;
}

pub(crate) struct CgroupV1 {}

impl CgroupFreeze for CgroupV1 {
    fn type_name(&self) -> &'static str {
        "cgroup"
    }

    fn freeze_path(&self) -> &'static str {
        formatcp!("{CGROUP_MOUNT_PATH}/freezer/freezer.state")
    }

    fn freeze_command(&self) -> &'static str {
        "FROZEN"
    }

    fn unfreeze_command(&self) -> &'static str {
        "THAWED"
    }
}

pub(crate) struct CgroupV2 {}

impl CgroupFreeze for CgroupV2 {
    fn type_name(&self) -> &'static str {
        "cgroup2"
    }

    fn freeze_path(&self) -> &'static str {
        formatcp!("{CGROUP_MOUNT_PATH}/cgroup.freeze")
    }

    fn freeze_command(&self) -> &'static str {
        "1"
    }

    fn unfreeze_command(&self) -> &'static str {
        "0"
    }
}

#[enum_dispatch(CgroupFreeze)]
pub(crate) enum Cgroup {
    V1(CgroupV1),
    V2(CgroupV2),
}

/// Checks which cgroup is being used and returns the `Cgroup` enum
pub(crate) fn get_cgroup() -> Result<Cgroup> {
    // if `/sys/fs/cgroup.controllers` exists, it means we're v2
    if Path::new("/sys/fs/cgroup.controllers").exists() {
        Ok(Cgroup::V2(CgroupV2 {}))
    } else {
        Ok(Cgroup::V1(CgroupV1 {}))
    }
}
