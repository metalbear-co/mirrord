/// Module for stolen traffic (intercepted?) handling
use tokio::{
    select,
    sync::mpsc::{channel, Receiver, Sender},
};
use mirrord_protocol::NewTCPConnection;

const CHANNEL_SIZE: usize = 1024;

pub enum TrafficIn {
    NewConnection(NewTCPConnection)
}

pub enum TrafficOut {}



impl TrafficControl {
}

/// Users shuold call create then start, then use TrafficControl returned from create to manage it.
impl TrafficManager {
    pub async fn start(mut self) {
        self.running = true;
        while running {
            select! {
                msg = self.incoming.recv() => {self.handle_incoming_message(msg).await?;},
            }
        }
    }

    async fn handle_incoming_message(msg: Option<TrafficIn>) -> Result<()> {
        match msg {
            None => {self.running = false},
            Some(TrafficIn::)
        }
        Ok(())
    }

}
