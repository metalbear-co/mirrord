use std::{
    fs::File,
    os::unix::prelude::{IntoRawFd, RawFd},
};

use anyhow::Result;
use async_trait::async_trait;
use nix::sched::setns;
use serde::{Deserialize, Serialize};

#[async_trait]
pub trait Namespace {
    async fn get_namespace(&self) -> Result<String>;
    fn set_namespace(&self, ns_path: String) -> Result<()> {
        let fd: RawFd = File::open(ns_path)?.into_raw_fd();
        setns(fd, nix::sched::CloneFlags::CLONE_NEWNET)?;
        Ok(())
    }
}


#[derive(Serialize, Deserialize, Debug)]
pub(crate) struct NamespaceInfo {
    #[serde(rename = "type")]
    pub ns_type: String,
    pub path: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
pub(crate) struct LinuxInfo {
    pub namespaces: Vec<NamespaceInfo>,
}

#[derive(Serialize, Deserialize, Debug)]
pub(crate) struct Spec {
    pub linux: LinuxInfo,
}