/// TCP Traffic management, common code for stealing & mirroring
use mirrord_protocol::{NewTCPConnection, TCPClose, TCPData};
use tokio::{
    select,
    sync::mpsc::{channel, Receiver, Sender},
};

const CHANNEL_SIZE: usize = 1024;

pub enum TrafficIn {
    NewConnection(NewTCPConnection),
    TCPData(TCPData),
    TCPClose(TCPClose),
}

/// Struct responsible for managing the traffic stealing.
/// Communicates using incoming/outgoing channels.
pub struct TCPConfig {
    outgoing: Sender<TrafficOut>,
    incoming: Receiver<TrafficIn>,
}

/// Struct for controlling the traffic stealing struct.
pub struct TCPApi {
    outgoing: Sender<TrafficIn>,
    incoming: Receiver<TrafficOut>,
}

impl TCPApi {
    pub async fn send(&self, msg: TrafficIn) -> Result<()> {
        self.outgoing.send(msg).await
    }

    pub async fn recv(&self) -> impl Future<Option<TrafficOut>> {
        self.incoming.recv()
    }
}

pub trait TCPHandler {
    /// Create new TCP handler communicating using given TCPConfig
    pub fn new(config: TCPConfig) -> impl TCPHandler;

    /// Run the TCP Handler, usually as a spawned task.
    pub async fn run(mut self) -> Result<()> {
        let config = self.config();
        while self.is_running() {
            select! {
                msg = config.incoming.recv() => {self.handle_incoming_message(msg).await?;},
            }
        }
    }

    /// Should the run loop keep running
    pub fn is_running(&self) -> bool;

    /// Changes the state so is_running will return False
    pub fn stop_running(&self);

    pub fn config(&mut self) -> &mut TCPConfig;

    async fn handle_incoming_message(&self, msg: Option<TrafficIn>) -> Result<()> {
        match msg {
            None => self.stop_running(),
            NewConnection(conn) => self.handle_new_connection(conn).await?,
            TCPData(data) => self.handle_new_data(data).await?,
            TCPClose(close) => self.handle_close().await?,
        }
        Ok(())
    }

    /// Handle NewConnection messages
    async fn handle_new_connection(&self, conn: NewTCPConnection) -> Result<()>;

    /// Handle New Data messages
    async fn handle_new_data(&self, data: TCPData) -> Result<()>;

    /// Handle connection close
    async fn handle_close(&self, close: TCPClose) -> Result<()>;
}

pub fn create_tcp_handler<T>() -> (T, TCPApi)
where
    T: TCPHandler,
{
    let (traffic_in_tx, traffic_in_rx) = channel(CHANNEL_SIZE);
    let (traffic_out_tx, traffic_out_rx) = channel(CHANNEL_SIZE);
    let handler = T::new(TCPConfig {
        outgoing: traffic_out_tx,
        incoming: traffic_in_rx,
    });
    let control = TrafficControl {
        incoming: traffic_out_rx,
        outgoing: traffic_in_tx,
    };
    (handler, control)
}
