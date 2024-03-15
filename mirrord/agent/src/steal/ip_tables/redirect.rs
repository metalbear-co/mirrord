use async_trait::async_trait;
use enum_dispatch::enum_dispatch;
use mirrord_protocol::Port;

use crate::error::Result;

#[async_trait]
#[enum_dispatch]
pub(crate) trait Redirect {
    async fn mount_entrypoint(&self) -> Result<()>;

    async fn unmount_entrypoint(&self) -> Result<()>;

    /// Create port redirection
    async fn add_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()>;
    /// Remove port redirection
    async fn remove_redirect(&self, redirected_port: Port, target_port: Port) -> Result<()>;
}
