use async_trait::async_trait;
use enum_dispatch::enum_dispatch;

use crate::error::IPTablesResult;

#[async_trait]
#[enum_dispatch]
pub(crate) trait Redirect {
    async fn mount_entrypoint(&self) -> IPTablesResult<()>;

    async fn unmount_entrypoint(&self) -> IPTablesResult<()>;

    /// Create port redirection
    async fn add_redirect(&self, redirected_port: u16, target_port: u16) -> IPTablesResult<()>;
    /// Remove port redirection
    async fn remove_redirect(&self, redirected_port: u16, target_port: u16) -> IPTablesResult<()>;
}
