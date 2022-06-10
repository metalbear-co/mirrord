use std::{
    collections::HashSet,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
};

/// TCP Traffic management, common code for stealing & mirroring
use async_trait::async_trait;
use mirrord_protocol::{NewTCPConnection, TCPClose, TCPData};
use tokio::{net::TcpStream, sync::mpsc::Sender};

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

    pub async fn new_tcp_connection(&self, conn: NewTCPConnection) -> Result<(), LayerError> {
        self.send(TrafficHandlerInput::NewConnection(conn)).await
    }

    pub async fn tcp_data(&self, data: TCPData) -> Result<(), LayerError> {
        self.send(TrafficHandlerInput::Data(data)).await
    }

    pub async fn tcp_close(&self, close: TCPClose) -> Result<(), LayerError> {
        self.send(TrafficHandlerInput::Close(close)).await
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
        match message {
            TrafficHandlerInput::NewConnection(conn) => self.handle_new_connection(conn).await,
            TrafficHandlerInput::Data(data) => self.handle_new_data(data).await,
            TrafficHandlerInput::Close(close) => self.handle_close(close),
            TrafficHandlerInput::Listen(listen) => self.handle_listen(listen),
        }
    }

    /// Handle NewConnection messages
    async fn handle_new_connection(&mut self, conn: NewTCPConnection) -> Result<(), LayerError>;

    /// Connects to the local listening socket, add it to the queue and return the stream.
    /// Find better name
    async fn create_local_stream(
        &mut self,
        conn: &NewTCPConnection,
    ) -> Result<TcpStream, LayerError> {
        let destination_port = conn.destination_port;

        let listen_data = self
            .ports()
            .get(&destination_port)
            .ok_or(LayerError::PortNotFound(destination_port))?;

        let addr = match listen_data.ipv6 {
            false => SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), listen_data.real_port),
            true => SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), listen_data.real_port),
        };

        let info = SocketInformation::new(SocketAddr::new(conn.address, conn.source_port));
        {
            CONNECTION_QUEUE.lock().unwrap().add(&listen_data.fd, info);
        }

        TcpStream::connect(addr).await.map_err(From::from)
    }

    /// Handle New Data messages
    async fn handle_new_data(&mut self, data: TCPData) -> Result<(), LayerError>;

    /// Handle connection close
    fn handle_close(&mut self, close: TCPClose) -> Result<(), LayerError>;

    /// Handle listen request
    fn handle_listen(&mut self, listen: Listen) -> Result<(), LayerError> {
        self.ports_mut()
            .insert(listen)
            .then_some(())
            .ok_or(LayerError::ListenAlreadyExists)
    }
}
