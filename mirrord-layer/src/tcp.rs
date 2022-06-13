use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

/// TCP Traffic management, common code for stealing & mirroring
use async_trait::async_trait;
use mirrord_protocol::{NewTCPConnection, TCPClose, TCPData};
use tokio::{net::TcpStream, sync::mpsc::Sender};
use tracing::debug;

use crate::{
    common::Listen,
    error::LayerError,
    sockets::{SocketInformation, CONNECTION_QUEUE},
};

type TrafficHandlerInputSender = Sender<TrafficHandlerInput>;

#[derive(Debug)]
pub enum TrafficHandlerInput {
    Listen(Listen),
    NewConnection(NewTCPConnection),
    Data(TCPData),
    Close(TCPClose),
}

/// To be used by traffic stealer
// pub enum TrafficOut {}

/// Struct for controlling the traffic handler struct
pub struct TCPApi {
    outgoing: TrafficHandlerInputSender,
    // This is reserved for stealing API.
    // #[allow(dead_code)]
    // incoming: Receiver<TrafficOut>,
}

impl TCPApi {
    pub fn new(outgoing: TrafficHandlerInputSender) -> Self {
        Self { outgoing }
    }
    pub async fn send(&self, msg: TrafficHandlerInput) -> Result<(), LayerError> {
        self.outgoing.send(msg).await.map_err(From::from)
    }

    // This is reserved for stealing API.
    // #[allow(dead_code)]
    // pub async fn recv(&mut self) -> Option<TrafficOut> {
    //     self.incoming.recv().await
    // }

    pub async fn listen_request(&self, listen: Listen) -> Result<(), LayerError> {
        self.send(TrafficHandlerInput::Listen(listen)).await
    }

    pub async fn new_tcp_connection(
        &self,
        tcp_connection: NewTCPConnection,
    ) -> Result<(), LayerError> {
        self.send(TrafficHandlerInput::NewConnection(tcp_connection))
            .await
    }

    pub async fn tcp_data(&self, tcp_data: TCPData) -> Result<(), LayerError> {
        self.send(TrafficHandlerInput::Data(tcp_data)).await
    }

    pub async fn tcp_close(&self, tcp_close: TCPClose) -> Result<(), LayerError> {
        self.send(TrafficHandlerInput::Close(tcp_close)).await
    }
}

#[async_trait]
pub trait TCPHandler {
    fn ports(&self) -> &HashSet<Listen>;
    fn ports_mut(&mut self) -> &mut HashSet<Listen>;

    /// Returns true to let caller know to keep running
    async fn handle_incoming_message(
        &mut self,
        message: TrafficHandlerInput,
    ) -> Result<(), LayerError> {
        debug!("handle_incoming_message -> message {:#?}", message);

        let handled = match message {
            TrafficHandlerInput::NewConnection(tcp_connection) => {
                self.handle_new_connection(tcp_connection).await
            }
            TrafficHandlerInput::Data(tcp_data) => self.handle_new_data(tcp_data).await,
            TrafficHandlerInput::Close(tcp_close) => self.handle_close(tcp_close),
            TrafficHandlerInput::Listen(listen) => self.handle_listen(listen),
        };

        debug!("handle_incoming_message -> handled {:#?}", handled);

        handled
    }

    /// Handle NewConnection messages
    async fn handle_new_connection(&mut self, conn: NewTCPConnection) -> Result<(), LayerError>;

    /// Connects to the local listening socket, add it to the queue and return the stream.
    /// Find better name
    async fn create_local_stream(
        &mut self,
        tcp_connection: &NewTCPConnection,
    ) -> Result<TcpStream, LayerError> {
        let destination_port = tcp_connection.destination_port;

        let listen = self
            .ports()
            .get(&destination_port)
            .ok_or(LayerError::PortNotFound(destination_port))?;

        let addr: SocketAddr = listen.into();

        let info = SocketInformation::new(SocketAddr::new(
            tcp_connection.address,
            tcp_connection.source_port,
        ));
        {
            CONNECTION_QUEUE.lock().unwrap().add(&listen.fd, info);
        }

        TcpStream::connect(addr).await.map_err(From::from)
    }

    /// Handle New Data messages
    async fn handle_new_data(&mut self, data: TCPData) -> Result<(), LayerError>;

    /// Handle connection close
    fn handle_close(&mut self, close: TCPClose) -> Result<(), LayerError>;

    /// Handle listen request
    fn handle_listen(&mut self, listen: Listen) -> Result<(), LayerError> {
        debug!("handle_listen -> ");

        self.ports_mut()
            .insert(listen)
            .then_some(())
            .ok_or(LayerError::ListenAlreadyExists)
    }
}
