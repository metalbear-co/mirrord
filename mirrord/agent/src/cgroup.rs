//! Logic for pausing using cgroup directly
//! Pause requires privileged - assumes ephemeral (hardcoded pid 1)
use std::{
    fs::OpenOptions,
    io::{BufRead, Write},
    path::{Path, PathBuf},
};

use const_format::formatcp;
use enum_dispatch::enum_dispatch;
use nix::mount::{mount, MsFlags};
use tracing::trace;

use crate::{
    error::{AgentError, Result},
    namespace::{set_namespace, NamespaceType},
};

const CGROUP_MOUNT_PATH: &str = "/mirrord_cgroup";

/// Trait for objects that can be paused
#[enum_dispatch]
pub(crate) trait CgroupFreeze {
    fn pause(&self) -> Result<()>;
    fn unpause(&self) -> Result<()>;
}

#[derive(Debug)]
pub(crate) struct CgroupV1 {
    /// Path to the the process' cgroup - usually /kubepods/besteffort/ID
    cgroup_path: PathBuf,
}

impl CgroupV1 {
    #[tracing::instrument(level = "trace", ret)]
    pub(crate) fn new() -> Result<Self> {
        let file = std::fs::File::open("/proc/1/cgroup")?;
        let reader = std::io::BufReader::new(file).lines();
        for line in reader {
            let mut line_iter = line?.split(':');
            // we don't care about the number, prefer the string for comparison
            line_iter.next().ok_or(AgentError::PauseFailedCgroup(
                "malformed ID cgroup v1 file".to_string(),
            ))?;
            let cgroup_type = line_iter.next().ok_or(AgentError::PauseFailedCgroup(
                "malformed cgroup type cgroup v1 file".to_string(),
            ))?;
            if cgroup_type == "freezer" {
                let cgroup_path = line_iter.next().ok_or(AgentError::PauseFailedCgroup(
                    "malformed path cgroup v1 file".to_string(),
                ))?;
                return Ok(Self {
                    cgroup_path: PathBuf::from(cgroup_path),
                });
            }
        }
        Err(AgentError::PauseFailedCgroup(
            "no freezer cgroup found".to_string(),
        ))
    }
}

const CGROUP_V1_FREEZE_PATH: &str = "freezer.state";

/// We mkdir, mount the cgroup, which mounts the root for some reason
/// then find the cgroup path, join it and write freeze to it.
impl CgroupFreeze for CgroupV1 {
    #[tracing::instrument(level = "trace", ret, skip(self))]
    fn pause(&self) -> Result<()> {
        // Check if our cgroup is mounted, if not, mount it.
        let cgroup_path = Path::new(CGROUP_MOUNT_PATH);
        if !cgroup_path.exists() {
            trace!("mounting cgroup");
            std::fs::create_dir(cgroup_path)?;
            mount(
                Some("cgroup"),
                cgroup_path,
                Some("cgroup"),
                MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC | MsFlags::MS_NODEV,
                "freezer",
            )?;
        }

        let mut open_options = OpenOptions::new();
        let mut file = open_options
            .write(true)
            .open(self.cgroup_path.join(CGROUP_V1_FREEZE_PATH))?;
        file.write_all("FROZEN".as_bytes())?;
        Ok(())
    }

    #[tracing::instrument(level = "trace", ret, skip(self))]
    fn unpause(&self) -> Result<()> {
        // if we're unpausing, mount should exist and we should be in the cgroup namespace
        let mut open_options = OpenOptions::new();
        let mut file = open_options
            .write(true)
            .open(self.cgroup_path.join(CGROUP_V1_FREEZE_PATH))?;
        file.write_all("THAWED".as_bytes())?;
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct CgroupV2 {}

const CGROUP_V2_FREEZE_PATH: &str = formatcp!("{CGROUP_MOUNT_PATH}/cgroup.freeze");

/// mkdir, nsenter the target pid's cgroup then mount the cgroup, write freeze.
impl CgroupFreeze for CgroupV2 {
    #[tracing::instrument(level = "trace", ret, skip(self))]
    fn pause(&self) -> Result<()> {
        // Enter the namespace, we might be already in it but it doesn't really matter
        // Note: Entering the cgroup namespace **doesn't** set put our process in the cgroup :phew:
        set_namespace(1, NamespaceType::Cgroup)?;
        // Check if our cgroup is mounted, if not, mount it.
        let cgroup_path = Path::new(CGROUP_MOUNT_PATH);
        if !cgroup_path.exists() {
            trace!("mounting cgroup");
            std::fs::create_dir(cgroup_path)?;
            mount(
                None::<&str>,
                cgroup_path,
                Some("cgroup2"),
                MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC | MsFlags::MS_NODEV,
                None::<&str>,
            )?;
        }
        let mut open_options = OpenOptions::new();
        let mut file = open_options.write(true).open(CGROUP_V2_FREEZE_PATH)?;
        file.write_all("1".as_bytes())?;
        Ok(())
    }

    #[tracing::instrument(level = "trace", ret, skip(self))]
    fn unpause(&self) -> Result<()> {
        // if we're unpausing, mount should exist and we should be in the cgroup namespace
        let mut open_options = OpenOptions::new();
        let mut file = open_options.write(true).open(CGROUP_V2_FREEZE_PATH)?;
        file.write_all("0".as_bytes())?;
        Ok(())
    }
}

/// V1 Docs: https://docs.kernel.org/admin-guide/cgroup-v1/index.html
/// V2 Docs: https://docs.kernel.org/admin-guide/cgroup-v2.html
#[enum_dispatch(CgroupFreeze)]
#[derive(Debug)]
pub(crate) enum Cgroup {
    V1(CgroupV1),
    V2(CgroupV2),
}

/// Checks which cgroup is being used and returns the `Cgroup` enum
#[tracing::instrument(level = "trace", ret)]
pub(crate) fn get_cgroup() -> Result<Cgroup> {
    // if `/sys/fs/cgroup.controllers` exists, it means we're v2
    if Path::new("/sys/fs/cgroup/cgroup.controllers").exists() {
        Ok(Cgroup::V2(CgroupV2 {}))
    } else {
        Ok(Cgroup::V1(CgroupV1::new()?))
    }
}
