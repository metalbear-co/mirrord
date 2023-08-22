//! Logic for pausing using cgroup directly
//! Pause requires privileged - assumes ephemeral (hardcoded pid 1)
use std::{path::Path, fs::{File, OpenOptions}, io::Write};

use enum_dispatch::enum_dispatch;
use nix::mount::{mount, MsFlags};

use crate::error::Result;

const CGROUP_MOUNT_PATH: &str = "/mirrord_cgroup";

/// Trait for objects that can be paused
#[enum_dispatch]
pub(crate) trait CgroupFreeze {
    fn pause(&self) -> Result<()>;
    fn unpause(&self) -> Result<()>;
}

struct CgroupV1 {}

impl CgroupFreeze for CgroupV1 {
    fn pause(&self) -> Result<()> {
        // Enter the namespace, we might be already in it but it doesn't really matter
        // Note: Entering the cgroup namespace **doesn't** set put our process in the cgroup :phew:
        set_namespace(1, NamespaceType::Cgroup)?;
        // Check if our cgroup is mounted, if not, mount it.
        let cgroup_path = Path::new(CGROUP_MOUNT_PATH);
        if !cgroup_path.exists() {
            mount(None, cgroup_path, Some("cgroup"), MsFlags::empty(), None)?;
        }
        let open_options = OpenOptions::new().write(true);
        let file = open_options.open(cgroup_path.join("freezer").join("freezer.state"))?;
        file.write_all("FROZEN".as_bytes())?;

        Ok(())
    }

    fn unpause(&self) -> Result<()> {
        // if we're unpausing, mount should exist and we should be in the cgroup namespace
        let cgroup_path = Path::new(CGROUP_MOUNT_PATH);
        let open_options = OpenOptions::new().write(true);
        let mut file = open_options.open(cgroup_path.join("freezer").join("freezer.state"))?;
        file.write_all("THAWED".as_bytes())?;
        Ok(())
    }
}

struct CgroupV2 {}

impl CgroupFreeze for CgroupV2 {
    fn pause(&self) -> Result<()> {
        unimplemented!()
    }

    fn unpause(&self) -> Result<()> {
        unimplemented!()
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