//! Logic for pausing using cgroup directly
//! Pause requires privileged 
use std::path::Path;

use crate::error::Result;
use enum_dispatch::enum_dispatch;

/// Trait for objects that can be paused
#[enum_dispatch]
pub(crate) trait CgroupFreeze {
    fn pause(&self) -> Result<()>;
    fn unpause(&self) -> Result<()>;
}

struct CgroupV1 {}

impl CgroupFreeze for CgroupV1 {
    fn pause(&self) -> Result<()> {
        unimplemented!()
    }

    fn unpause(&self) -> Result<()> {
        unimplemented!()
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