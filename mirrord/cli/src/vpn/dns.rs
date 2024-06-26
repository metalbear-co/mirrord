use std::net::SocketAddr;

use futures::{stream::FuturesUnordered, StreamExt};
use hickory_proto::op::Message;
use tokio::{
    net::UdpSocket,
    sync::{mpsc, oneshot},
};

pub type DnsRequest = (
    (Message, SocketAddr),
    oneshot::Sender<(Message, SocketAddr)>,
);

#[derive(Debug)]
pub struct DnsServer {
    socket: UdpSocket,
}

impl DnsServer {
    pub fn new(socket: UdpSocket) -> Self {
        DnsServer { socket }
    }

    pub fn port(&self) -> Option<u16> {
        self.socket.local_addr().map(|socket| socket.port()).ok()
    }

    pub fn start(self) -> mpsc::Receiver<DnsRequest> {
        let DnsServer { socket } = self;
        let (tx, rx) = mpsc::channel::<DnsRequest>(128);

        tokio::spawn(async move {
            let mut buf = [0; 4096];
            let mut replies = FuturesUnordered::<oneshot::Receiver<(Message, SocketAddr)>>::new();

            loop {
                tokio::select! {
                    conn = socket.recv_from(&mut buf) => {
                        match conn {
                            Ok((len, addr)) => {
                                println!("dns message: len {len}");

                                let Some(buf) = buf.get(..len) else {
                                    continue;
                                };

                                let Ok(message) = Message::from_vec(buf) else {
                                    continue;
                                };

                                let (sender, receiver) = oneshot::channel();

                                if tx.send(((message, addr), sender)).await.is_err() {
                                    continue;
                                }

                                replies.push(receiver);
                            }
                            Err(err) => {
                                eprintln!("{err}");

                                break;
                            }
                        }
                    }

                    message = replies.next(), if !replies.is_empty() => {
                        let Some(Ok((message, addr))) = message else {
                            continue;
                        };

                        let Ok(message) = message.to_vec() else {
                            continue;
                        };

                        println!("dns reply: len {}", message.len());

                        if let Err(err) = socket.send_to(&message, addr).await {
                            println!("{err:?}");
                        }
                    }
                }
            }

            drop(tx);
        });

        rx
    }
}
