use async_trait::async_trait;
use enum_dispatch::enum_dispatch;
use mirrord_protocol::Port;

use crate::error::AgentResult;

#[async_trait]
#[enum_dispatch]
pub(crate) trait Redirect {
    async fn mount_entrypoint(&self) -> AgentResult<()>;

    async fn unmount_entrypoint(&self) -> AgentResult<()>;

    /// Create port redirection
    async fn add_redirect(&self, redirected_port: Port, target_port: Port) -> AgentResult<()>;
    /// Remove port redirection
    async fn remove_redirect(&self, redirected_port: Port, target_port: Port) -> AgentResult<()>;
}
