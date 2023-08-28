//! Logic for pausing using cgroup directly
//! Pause requires privileged - assumes ephemeral (hardcoded pid 1)
use std::path::{Path, PathBuf};

use nix::mount::{mount, MsFlags};
use tokio::{
    fs::{create_dir, try_exists, File, OpenOptions},
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
};
use tracing::{trace, warn};

use crate::{
    error::{AgentError, Result},
    namespace::{set_namespace, NamespaceType},
};

const CGROUP_MOUNT_PATH: &str = "/mirrord_cgroup";
const CGROUP_SUBGROUP_MOUNT_PATH: &str = "/mirrord_cgroup/subgroup";

#[derive(Debug)]
pub(crate) struct CgroupV1 {
    /// Path to the the process' cgroup - usually /kubepods/besteffort/ID
    cgroup_path: PathBuf,
}

/// We mkdir, mount the cgroup, which mounts the root for some reason
/// then find the cgroup path, join it and write freeze to it.
impl CgroupV1 {
    const FREEZE_PATH: &str = "freezer.state";

    #[tracing::instrument(level = "trace", ret)]
    pub(crate) async fn new() -> Result<Self> {
        let file = File::open("/proc/1/cgroup")
            .await
            .map_err(|_| AgentError::PauseFailedCgroup("opening proc cgroup failed".to_string()))?;
        let mut reader = BufReader::new(file).lines();
        while let Some(line) = reader.next_line().await? {
            let mut line_iter = line.split(':');
            // we don't care about the number, prefer the string for comparison
            line_iter.next().ok_or(AgentError::PauseFailedCgroup(
                "malformed ID cgroup v1 file".to_string(),
            ))?;
            let cgroup_type = line_iter.next().ok_or_else(|| {
                AgentError::PauseFailedCgroup("malformed cgroup type cgroup v1 file".to_string())
            })?;
            if cgroup_type == "freezer" {
                let cgroup_path = line_iter.next().ok_or_else(|| {
                    AgentError::PauseFailedCgroup("malformed path cgroup v1 file".to_string())
                })?;
                return Ok(Self {
                    // strip / since joining "/a" and "/b" results in "/b"
                    cgroup_path: Path::new(CGROUP_MOUNT_PATH).join(PathBuf::from(
                        cgroup_path.strip_prefix('/').unwrap_or(cgroup_path),
                    )),
                });
            }
        }
        Err(AgentError::PauseFailedCgroup(
            "no freezer cgroup found".to_string(),
        ))
    }

    #[tracing::instrument(level = "trace", ret, skip(self))]
    async fn pause(&self) -> Result<()> {
        // Check if our cgroup is mounted, if not, mount it.
        let cgroup_path = Path::new(CGROUP_MOUNT_PATH);
        if !try_exists(cgroup_path).await? {
            trace!("mounting cgroup");
            create_dir(cgroup_path).await.map_err(|_| {
                AgentError::PauseFailedCgroup("create dir failed cgroupv1".to_string())
            })?;
            mount(
                Some("cgroup"),
                cgroup_path,
                Some("cgroup"),
                MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC | MsFlags::MS_NODEV,
                Some("freezer"),
            )
            .map_err(|_| AgentError::PauseFailedCgroup("mount failed cgroupv1".to_string()))?;
        }

        let mut open_options = OpenOptions::new();
        let mut file = open_options
            .write(true)
            .open(self.cgroup_path.join(Self::FREEZE_PATH))
            .await
            .map_err(|_| AgentError::PauseFailedCgroup("open file cgroupv1 failed".to_string()))?;
        file.write_all("FROZEN".as_bytes())
            .await
            .map_err(|_| AgentError::PauseFailedCgroup("writing frozen failed".to_string()))?;
        Ok(())
    }

    #[tracing::instrument(level = "trace", ret, skip(self))]
    async fn unpause(&self) -> Result<()> {
        // if we're unpausing, mount should exist and we should be in the cgroup namespace
        let mut open_options = OpenOptions::new();
        let mut file = open_options
            .write(true)
            .open(self.cgroup_path.join(Self::FREEZE_PATH))
            .await?;
        file.write_all("THAWED".as_bytes()).await?;
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct CgroupV2 {}

/// Reads given path's "cgroup.procs" file and returns the pids in it
#[tracing::instrument(level = "trace", ret)]
async fn read_pids_cgroupv2(cgroup_path: &Path) -> Result<Vec<u64>> {
    let file_name = cgroup_path.join(CgroupV2::PROCS_FILE);
    let file = File::open(file_name).await?;
    let buf_reader = BufReader::new(file);
    let mut pids = Vec::new();
    let mut lines = buf_reader.lines();
    while let Some(line) = lines.next_line().await? {
        let pid = line.trim().parse().map_err(|_| {
            AgentError::PauseFailedCgroup(format!("failed to parse pid line {line}"))
        })?;
        pids.push(pid)
    }
    Ok(pids)
}

/// Write given pids to given path's "cgroup.procs" file
#[tracing::instrument(level = "trace", ret)]
async fn move_pids_to_cgroupv2(cgroup_path: &Path, pids: Vec<u64>) -> Result<()> {
    for pid in pids {
        tokio::fs::write(cgroup_path.join(CgroupV2::PROCS_FILE), format!("{pid}"))
            .await
            .map_err(|_| {
                AgentError::PauseFailedCgroup("write pid to subgroup failed".to_string())
            })?;
    }
    Ok(())
}

/// (Un)Freeze the given cgroup
#[tracing::instrument(level = "trace", ret)]
async fn freeze_cgroupv2(cgroup_path: &Path, on: bool) -> Result<()> {
    let mut open_options = OpenOptions::new();
    let freeze_path = cgroup_path.join("cgroup.freeze");
    trace!("freeze path {freeze_path:?}");
    let mut file = open_options
        .write(true)
        .open(&freeze_path)
        .await
        .map_err(|e| AgentError::PauseFailedCgroup(format!("{e} open cgroup v2 failed")))?;

    let command = if on { "1" } else { "0" };
    file.write_all(command.as_bytes())
        .await
        .map_err(|_| AgentError::PauseFailedCgroup(format!("writing 1 failed {freeze_path:?}")))
}

impl CgroupV2 {
    const PROCS_FILE: &str = "cgroup.procs";

    #[tracing::instrument(level = "trace", ret, skip(self))]
    async fn pause(&self) -> Result<()> {
        // Enter the namespace, we might be already in it but it doesn't really matter
        // Note: Entering the cgroup namespace **doesn't** set put our process in the cgroup :phew:
        set_namespace(1, NamespaceType::Cgroup).map_err(|_| {
            AgentError::PauseFailedCgroup("set_namespace failed cgroup v2".to_string())
        })?;
        // Check if our cgroup is mounted, if not, mount it.
        let cgroup_path = Path::new(CGROUP_MOUNT_PATH);
        if !try_exists(cgroup_path).await? {
            trace!("mounting cgroup");
            create_dir(cgroup_path).await.map_err(|_| {
                AgentError::PauseFailedCgroup("create dir cgroup v2 failed".to_string())
            })?;
            mount(
                None::<&str>,
                cgroup_path,
                Some("cgroup2"),
                MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC | MsFlags::MS_NODEV,
                None::<&str>,
            )
            .map_err(|_| AgentError::PauseFailedCgroup("mount cgroup v2 failed".to_string()))?;
        }
        // On nsdelegate hosts (where cgroup2 is mounted with nsdelegate option),
        // we can't write to the cgroup.freeze file directly
        // so we have to create a sub group, move process there then we can freeze it.
        // In theory we could just create a brand new one, move the process into it but then
        // other cgroup rules won't affect it, so we want it to be under the same hierarchy.
        let sub_cgroup_path = Path::new(CGROUP_SUBGROUP_MOUNT_PATH);
        if !try_exists(sub_cgroup_path).await? {
            trace!("creating subgroup");
            create_dir(sub_cgroup_path).await.map_err(|_| {
                AgentError::PauseFailedCgroup("create dir subgroup failed".to_string())
            })?;
        }

        // Move cgroup processes' to the subgroup
        let pids = read_pids_cgroupv2(cgroup_path).await?;
        if pids.is_empty() {
            warn!("No pids found in cgroup v2 {cgroup_path:?}");
            return Err(AgentError::PauseFailedCgroup("no pids found".to_string()));
        }

        move_pids_to_cgroupv2(sub_cgroup_path, pids).await?;

        freeze_cgroupv2(sub_cgroup_path, true).await
    }

    #[tracing::instrument(level = "trace", ret, skip(self))]
    async fn unpause(&self) -> Result<()> {
        // if we're unpausing, mount should exist and we should be in the cgroup namespace
        // do reverse order - first unfreeze then move back to root cgroup
        // Move cgroup processes' to the subgroup
        let cgroup_path = Path::new(CGROUP_MOUNT_PATH);
        let sub_cgroup_path = Path::new(CGROUP_SUBGROUP_MOUNT_PATH);

        freeze_cgroupv2(sub_cgroup_path, false).await?;

        let pids = read_pids_cgroupv2(sub_cgroup_path).await?;

        if pids.is_empty() {
            warn!("No pids found in cgroup v2 {cgroup_path:?}");
            return Err(AgentError::PauseFailedCgroup("no pids found".to_string()));
        }

        move_pids_to_cgroupv2(cgroup_path, pids).await
    }
}

/// V1 Docs: https://docs.kernel.org/admin-guide/cgroup-v1/index.html
/// V2 Docs: https://docs.kernel.org/admin-guide/cgroup-v2.html
#[derive(Debug)]
pub(crate) enum Cgroup {
    V1(CgroupV1),
    V2(CgroupV2),
}

impl Cgroup {
    pub(crate) async fn new() -> Result<Self> {
        // if `/sys/fs/cgroup.controllers` exists, it means we're v2
        if tokio::fs::try_exists("/sys/fs/cgroup/cgroup.controllers").await? {
            Ok(Self::V2(CgroupV2 {}))
        } else {
            Ok(Self::V1(CgroupV1::new().await?))
        }
    }

    pub(crate) async fn pause(&self) -> Result<()> {
        match self {
            Cgroup::V1(cgroup) => cgroup.pause().await,
            Cgroup::V2(cgroup) => cgroup.pause().await,
        }
    }

    pub(crate) async fn unpause(&self) -> Result<()> {
        match self {
            Cgroup::V1(cgroup) => cgroup.unpause().await,
            Cgroup::V2(cgroup) => cgroup.unpause().await,
        }
    }
}
