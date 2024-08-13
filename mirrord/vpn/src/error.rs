use mirrord_protocol::ClientMessage;
use thiserror::Error;
use tokio::sync::mpsc;

#[derive(Debug, Error)]
pub enum VpnError {
    #[error("vpn failed becuase of bad response from agent: {0}")]
    AgentErrorResponse(#[from] mirrord_protocol::ResponseError),

    #[error("vpn failed becuase of unexpectec response from agent")]
    AgentUnexpcetedResponse,

    #[error("expected an agent response, receiver channel dropped")]
    AgentNoResponse,

    #[error("vpn failed to setup nececery overrides: {0}")]
    SetupIO(std::io::Error),

    #[error("unable to send client message to agent, sender channel dropped")]
    ClientMessageDropped(#[from] mpsc::error::SendError<ClientMessage>),
}
