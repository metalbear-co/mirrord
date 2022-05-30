use std::{fs::File, os::unix::prelude::{IntoRawFd, RawFd}};

use anyhow::Result;
use async_trait::async_trait;
use nix::sched::setns;

#[async_trait]
pub trait Namespace {
    async fn get_namespace(&mut self) -> Result<String>;
    fn set_namespace(&self, ns_path: String) -> Result<()> {
        let fd: RawFd = File::open(ns_path)?.into_raw_fd();
        setns(fd, nix::sched::CloneFlags::CLONE_NEWNET)?;       
        Ok(())
    }
}
