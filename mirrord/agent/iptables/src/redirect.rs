use async_trait::async_trait;
use enum_dispatch::enum_dispatch;

use crate::error::IPTablesResult;

#[async_trait]
#[enum_dispatch]
pub trait Redirect {
    async fn mount_entrypoint(&self) -> IPTablesResult<()>;

    /// Returns an error if any of the deletions failed. Deletions fail also if what they try
    /// to delete does not exist. It's up to the caller of this method to decide how to deal with
    /// that.
    async fn unmount_entrypoint(&self) -> IPTablesResult<()>;

    /// Create port redirection
    async fn add_redirect(&self, redirected_port: u16, target_port: u16) -> IPTablesResult<()>;
    /// Remove port redirection
    async fn remove_redirect(&self, redirected_port: u16, target_port: u16) -> IPTablesResult<()>;
}
