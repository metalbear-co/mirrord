use std::time::Duration;

use mirrord_protocol::{ClientMessage, DaemonMessage, CLIENT_READY_FOR_LOGS};
use tokio::{net::TcpStream, sync::mpsc::{Receiver, self}};

use crate::{
    agent_conn::{AgentCommunicationError, AgentConnection},
    error::{IntProxyError, Result},
    layer_conn::LayerConnection,
    ping_pong::PingPong,
    protocol::{LayerToProxyMessage, LocalMessage, ProxyToLayerMessage, NOT_A_RESPONSE},
    proxies::{outgoing::OutgoingProxy, simple::SimpleProxy},
    IntProxy, ProxyMessage,
};

pub struct ProxySession {
    main_rx: Receiver<ProxyMessage>,

    agent_conn: AgentConnection,
    layer_conn: LayerConnection,

    simple_proxy: SimpleProxy,
    outgoing_proxy: OutgoingProxy,
    ping_pong: PingPong,
}

impl ProxySession {
    const PING_INTERVAL: Duration = Duration::from_secs(30);
    const CHANNEL_SIZE: usize = 512;

    pub async fn new(intproxy: &IntProxy, conn: TcpStream) -> Result<Self> {
        let (main_tx, main_rx) = mpsc::channel(Self::CHANNEL_SIZE);

        let mut agent_conn =
            AgentConnection::new(&intproxy.config, intproxy.agent_connect_info.as_ref()).await?;
        agent_conn.ping_pong().await?;

        let layer_conn = LayerConnection::new(conn, Self::CHANNEL_SIZE);

        let simple_proxy = SimpleProxy::new(main_tx.clone());
        let outgoing_proxy = OutgoingProxy::new(main_tx.clone());
        let ping_pong = PingPong::new(Self::PING_INTERVAL, main_tx.clone());

        Ok(Self {
            main_rx,

            agent_conn,
            layer_conn,

            simple_proxy,
            outgoing_proxy,
            ping_pong,
        })
    }

    pub async fn serve_connection(mut self) -> Result<()> {
        self.agent_conn
            .sender()
            .send(ClientMessage::SwitchProtocolVersion(
                mirrord_protocol::VERSION.clone(),
            ))
            .await?;

        loop {
            tokio::select! {
                biased;

                msg = self.layer_conn.receive() => match msg {
                    Some(msg) => self.handle_layer_message(msg).await,
                    None => {
                        tracing::trace!("layer connection closed");
                        break Ok(());
                    }
                },

                msg = self.agent_conn.receive() => self.handle_agent_message(msg?).await?,

                Some(msg) = self.main_rx.recv() => match msg {
                    ProxyMessage::ToLayer(msg) => self.layer_conn.sender().send(msg).await?,
                    ProxyMessage::ToAgent(msg) => self.agent_conn.sender().send(msg).await?,
                },
            }
        }
    }

    async fn handle_agent_message(&mut self, message: DaemonMessage) -> Result<()> {
        self.ping_pong.handle_agent_message(&message).await;

        let res = match message {
            DaemonMessage::Pong => Ok(()),
            DaemonMessage::Close(reason) => {
                return Err(IntProxyError::AgentClosedConnection(reason))
            }
            DaemonMessage::TcpOutgoing(msg) => self.outgoing_proxy.handle_agent_stream_msg(msg).await,
            DaemonMessage::UdpOutgoing(msg) => self.outgoing_proxy.handle_agent_datagrams_msg(msg).await,
            DaemonMessage::File(msg) => self.simple_proxy.handle_file_res(msg).await,
            DaemonMessage::GetAddrInfoResponse(msg) => self.simple_proxy.handle_addr_info_res(msg).await,
            DaemonMessage::Tcp(..) => todo!(),
            DaemonMessage::TcpSteal(..) => todo!(),
            DaemonMessage::SwitchProtocolVersionResponse(protocol_version) => {
                if CLIENT_READY_FOR_LOGS.matches(&protocol_version) {
                    self.agent_conn
                        .sender()
                        .send(ClientMessage::ReadyForLogs)
                        .await?;
                }

                Ok(())
            }
            DaemonMessage::LogMessage(log) => {
                self.layer_conn
                    .sender()
                    .send(LocalMessage {
                        message_id: NOT_A_RESPONSE,
                        inner: ProxyToLayerMessage::AgentLog(log),
                    })
                    .await?;
                Ok(())
            }
            other => {
                return Err(IntProxyError::AgentCommunicationError(
                    AgentCommunicationError::UnexpectedMessage(other),
                ))
            }
        };

        if res.is_err() {
            tracing::error!("one of background tasks is down");
        }

        Ok(())
    }

    async fn handle_layer_message(&mut self, message: LocalMessage<LayerToProxyMessage>) {
        match message.inner {
            LayerToProxyMessage::NewSession(..) => todo!(),
            LayerToProxyMessage::File(req) => {
                self.simple_proxy.handle_file_req(req, message.message_id)
            }
            LayerToProxyMessage::GetAddrInfo(req) => self
                .simple_proxy
                .handle_addr_info_req(req, message.message_id),
            LayerToProxyMessage::OutgoingConnect(req) => {}
            LayerToProxyMessage::Incoming(..) => todo!(),
        }
    }
}
