//! Logic for pausing using cgroup directly
//! Pause requires privileged - assumes ephemeral (hardcoded pid 1)
use std::{
    path::{Path, PathBuf},
    str::FromStr,
};

use nix::mount::{mount, MsFlags};
use thiserror::Error;
use tokio::{
    fs::{create_dir, try_exists, File, OpenOptions},
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
};
use tracing::{trace, warn};

use crate::namespace::{set_namespace, NamespaceError, NamespaceType};

/// Errors that are common for both cgroup versions.
#[derive(Debug, Error)]
pub(crate) enum CgroupSharedError {
    #[error("Failed to check existence of mount: {0}")]
    MountExistsCheck(std::io::Error),
    #[error("Failed creating cgroup mount dir: {0}")]
    CreatingCgroupMountDir(std::io::Error),
    #[error("Failed mounting cgroup: {0}")]
    MountingCgroup(nix::Error),
    #[error("Failed opening freezer file: {0}")]
    OpeningFreezerFile(std::io::Error),
    #[error("Failed writing to freezer file: {0}")]
    WritingFreezerFile(std::io::Error),
}

#[derive(Debug, Error)]
pub(crate) enum CgroupV1Error {
    #[error("Failed to open pid's cgroup file: {0}")]
    FailedOpenPidCgroup(std::io::Error),
    #[error("Failed to read pid's cgroup file: {0}")]
    FailedReadingPidCgroupFile(std::io::Error),
    #[error("Malformed pid's cgroup file had unexpected line: {0}")]
    MalformedPidCgroupFile(String),
    #[error("Couldn't find freezer controller in pid's cgroup file")]
    NoFreezerController,
    #[error(transparent)]
    CgroupSharedError(#[from] CgroupSharedError),
}

#[derive(Debug, Error)]
pub(crate) enum CgroupV2Error {
    #[error("Failed to open cgroup.procs file: {0}")]
    OpenCgroupProcs(std::io::Error),
    #[error("Failed to read cgroup.procs file: {0}")]
    ReadingCgroupProcs(std::io::Error),
    #[error("Malformed cgroup.procs file: {0}")]
    MalformedCgroupProcs(<u64 as FromStr>::Err, String),
    #[error("Failed to write process to cgroup.procs file: {0}")]
    WritingCgroupProcs(std::io::Error),
    #[error("Failed entering cgroup namespace {0}")]
    EnteringCgroupNamespace(#[from] NamespaceError),
    #[error("Failed to check existence of cgroup subdir: {0}")]
    SubgroupDirExistsCheck(std::io::Error),
    #[error("Failed creating cgroup subdir: {0}")]
    CreatingSubgroupDir(std::io::Error),
    #[error("No pids found in cgroup")]
    NoPidsFoundInCgroup,
    #[error(transparent)]
    CgroupSharedError(#[from] CgroupSharedError),
}

#[derive(Debug, Error)]
pub(crate) enum CgroupError {
    #[error("cgroup v1 error: {0}")]
    CgroupV1Error(#[from] CgroupV1Error),
    #[error("cgroup v2 error: {0}")]
    CgroupV2Error(#[from] CgroupV2Error),
}

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
    const FREEZE_PATH: &'static str = "freezer.state";

    #[tracing::instrument(level = "trace", ret)]
    pub(crate) async fn new() -> Result<Self, CgroupV1Error> {
        let file = File::open("/proc/1/cgroup")
            .await
            .map_err(CgroupV1Error::FailedOpenPidCgroup)?;

        let mut reader = BufReader::new(file).lines();

        while let Some(line) = reader
            .next_line()
            .await
            .map_err(CgroupV1Error::FailedReadingPidCgroupFile)?
        {
            let mut line_iter = line.split(':');
            // we don't care about the number, prefer the string for comparison
            line_iter
                .next()
                .ok_or_else(|| CgroupV1Error::MalformedPidCgroupFile(line.clone()))?;
            let cgroup_type = line_iter
                .next()
                .ok_or_else(|| CgroupV1Error::MalformedPidCgroupFile(line.clone()))?;

            if cgroup_type == "freezer" {
                let cgroup_path = line_iter
                    .next()
                    .ok_or_else(|| CgroupV1Error::MalformedPidCgroupFile(line.clone()))?;
                return Ok(Self {
                    // strip / since joining "/a" and "/b" results in "/b"
                    cgroup_path: Path::new(CGROUP_MOUNT_PATH).join(PathBuf::from(
                        cgroup_path.strip_prefix('/').unwrap_or(cgroup_path),
                    )),
                });
            }
        }
        Err(CgroupV1Error::NoFreezerController)
    }

    #[tracing::instrument(level = "trace", ret, skip(self))]
    async fn pause(&self) -> Result<(), CgroupV1Error> {
        // Check if our cgroup is mounted, if not, mount it.
        let cgroup_path = Path::new(CGROUP_MOUNT_PATH);
        if !try_exists(cgroup_path)
            .await
            .map_err(CgroupSharedError::MountExistsCheck)?
        {
            trace!("mounting cgroup");
            create_dir(cgroup_path)
                .await
                .map_err(CgroupSharedError::CreatingCgroupMountDir)?;

            mount(
                Some("cgroup"),
                cgroup_path,
                Some("cgroup"),
                MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC | MsFlags::MS_NODEV,
                Some("freezer"),
            )
            .map_err(CgroupSharedError::MountingCgroup)?
        }

        let mut open_options = OpenOptions::new();
        let mut file = open_options
            .write(true)
            .open(self.cgroup_path.join(Self::FREEZE_PATH))
            .await
            .map_err(CgroupSharedError::OpeningFreezerFile)?;
        file.write_all("FROZEN".as_bytes())
            .await
            .map_err(CgroupSharedError::WritingFreezerFile)?;
        Ok(())
    }

    #[tracing::instrument(level = "trace", ret, skip(self))]
    async fn unpause(&self) -> Result<(), CgroupV1Error> {
        // if we're unpausing, mount should exist and we should be in the cgroup namespace
        let mut open_options = OpenOptions::new();
        let mut file = open_options
            .write(true)
            .open(self.cgroup_path.join(Self::FREEZE_PATH))
            .await
            .map_err(CgroupSharedError::OpeningFreezerFile)?;
        file.write_all("THAWED".as_bytes())
            .await
            .map_err(CgroupSharedError::WritingFreezerFile)?;
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) struct CgroupV2 {}

/// Reads given path's "cgroup.procs" file and returns the pids in it
#[tracing::instrument(level = "trace", ret)]
async fn read_pids_cgroupv2(cgroup_path: &Path) -> Result<Vec<u64>, CgroupV2Error> {
    let file_name = cgroup_path.join(CgroupV2::PROCS_FILE);
    let file = File::open(file_name)
        .await
        .map_err(CgroupV2Error::OpenCgroupProcs)?;
    let buf_reader = BufReader::new(file);
    let mut pids = Vec::new();
    let mut lines = buf_reader.lines();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(CgroupV2Error::ReadingCgroupProcs)?
    {
        let pid = line
            .trim()
            .parse()
            .map_err(|e| CgroupV2Error::MalformedCgroupProcs(e, line.clone()))?;
        pids.push(pid)
    }
    Ok(pids)
}

/// Write given pids to given path's "cgroup.procs" file
#[tracing::instrument(level = "trace", ret)]
async fn move_pids_to_cgroupv2(cgroup_path: &Path, pids: Vec<u64>) -> Result<(), CgroupV2Error> {
    let mut open_options = OpenOptions::new();
    let mut file = open_options
        .write(true)
        .open(cgroup_path.join(CgroupV2::PROCS_FILE))
        .await
        .map_err(CgroupV2Error::WritingCgroupProcs)?;

    for pid in pids {
        file.write_all(pid.to_string().as_bytes())
            .await
            .map_err(CgroupV2Error::WritingCgroupProcs)?;
    }
    Ok(())
}

/// (Un)Freeze the given cgroup
#[tracing::instrument(level = "trace", ret)]
async fn freeze_cgroupv2(cgroup_path: &Path, on: bool) -> Result<(), CgroupV2Error> {
    let mut open_options = OpenOptions::new();
    let freeze_path = cgroup_path.join("cgroup.freeze");
    trace!("freeze path {freeze_path:?}");
    let mut file = open_options
        .write(true)
        .open(&freeze_path)
        .await
        .map_err(CgroupSharedError::OpeningFreezerFile)?;

    let command = if on { "1" } else { "0" };
    file.write_all(command.as_bytes())
        .await
        .map_err(CgroupSharedError::WritingFreezerFile)?;
    Ok(())
}

impl CgroupV2 {
    const PROCS_FILE: &'static str = "cgroup.procs";

    #[tracing::instrument(level = "trace", ret, skip(self))]
    async fn pause(&self) -> Result<(), CgroupV2Error> {
        // Enter the namespace, we might be already in it but it doesn't really matter
        // Note: Entering the cgroup namespace **doesn't** set put our process in the cgroup :phew:
        set_namespace(1, NamespaceType::Cgroup)?;
        // Check if our cgroup is mounted, if not, mount it.
        let cgroup_path = Path::new(CGROUP_MOUNT_PATH);
        if !try_exists(cgroup_path)
            .await
            .map_err(CgroupSharedError::MountExistsCheck)?
        {
            trace!("mounting cgroup");
            create_dir(cgroup_path)
                .await
                .map_err(CgroupSharedError::CreatingCgroupMountDir)?;
            mount(
                None::<&str>,
                cgroup_path,
                Some("cgroup2"),
                MsFlags::MS_NOSUID | MsFlags::MS_NOEXEC | MsFlags::MS_NODEV,
                None::<&str>,
            )
            .map_err(CgroupSharedError::MountingCgroup)?;
        }
        // On nsdelegate hosts (where cgroup2 is mounted with nsdelegate option),
        // we can't write to the cgroup.freeze file directly
        // so we have to create a sub group, move process there then we can freeze it.
        // In theory we could just create a brand new one, move the process into it but then
        // other cgroup rules won't affect it, so we want it to be under the same hierarchy.
        let sub_cgroup_path = Path::new(CGROUP_SUBGROUP_MOUNT_PATH);
        if !try_exists(sub_cgroup_path)
            .await
            .map_err(CgroupV2Error::SubgroupDirExistsCheck)?
        {
            trace!("creating subgroup");
            create_dir(sub_cgroup_path)
                .await
                .map_err(CgroupV2Error::CreatingSubgroupDir)?;
        }

        // Move cgroup processes' to the subgroup
        let pids = read_pids_cgroupv2(cgroup_path).await?;
        if pids.is_empty() {
            warn!("No pids found in cgroup v2 {cgroup_path:?}");
            return Err(CgroupV2Error::NoPidsFoundInCgroup);
        }

        move_pids_to_cgroupv2(sub_cgroup_path, pids).await?;

        freeze_cgroupv2(sub_cgroup_path, true).await
    }

    #[tracing::instrument(level = "trace", ret, skip(self))]
    async fn unpause(&self) -> Result<(), CgroupV2Error> {
        // if we're unpausing, mount should exist and we should be in the cgroup namespace
        // do reverse order - first unfreeze then move back to root cgroup
        // Move cgroup processes' to the subgroup
        let cgroup_path = Path::new(CGROUP_MOUNT_PATH);
        let sub_cgroup_path = Path::new(CGROUP_SUBGROUP_MOUNT_PATH);

        freeze_cgroupv2(sub_cgroup_path, false).await?;

        let pids = read_pids_cgroupv2(sub_cgroup_path).await?;

        if pids.is_empty() {
            warn!("No pids found in cgroup v2 {cgroup_path:?}");
            return Err(CgroupV2Error::NoPidsFoundInCgroup);
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
    pub(crate) async fn new() -> Result<Self, CgroupError> {
        // if `/sys/fs/cgroup.controllers` exists, it means we're v2
        let v2 = tokio::fs::try_exists("/sys/fs/cgroup/cgroup.controllers")
            .await
            .unwrap_or(false);
        if v2 {
            Ok(Self::V2(CgroupV2 {}))
        } else {
            Ok(Self::V1(CgroupV1::new().await?))
        }
    }

    pub(crate) async fn pause(&self) -> Result<(), CgroupError> {
        match self {
            Cgroup::V1(cgroup) => cgroup.pause().await?,
            Cgroup::V2(cgroup) => cgroup.pause().await?,
        }
        Ok(())
    }

    pub(crate) async fn unpause(&self) -> Result<(), CgroupError> {
        match self {
            Cgroup::V1(cgroup) => cgroup.unpause().await?,
            Cgroup::V2(cgroup) => cgroup.unpause().await?,
        }
        Ok(())
    }
}
