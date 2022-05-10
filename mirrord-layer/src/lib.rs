// #![feature(c_variadic)]

use std::{sync::Mutex, thread, time::Duration};

use ctor::ctor;
use envconfig::Envconfig;
use frida_gum::{interceptor::Interceptor, Error, Gum, Module, NativePointer};
use futures::{SinkExt, StreamExt};
use kube::api::Portforwarder;
use lazy_static::lazy_static;
use mirrord_protocol::{ClientCodec, ClientMessage, DaemonMessage};
use tokio::{
    runtime::Runtime,
    select,
    sync::mpsc::{channel, Receiver, Sender},
};
use tracing::{debug, error};

mod config;
mod macros;
mod pod_api;
mod sockets;
use config::Config;
use tracing_subscriber::prelude::*;

lazy_static! {
    static ref GUM: Gum = unsafe { Gum::obtain() };
    static ref RUNTIME: Runtime = Runtime::new().unwrap();
    static ref SOCKETS: sockets::Sockets = sockets::Sockets::default();
    static ref NEW_CONNECTION_SENDER: Mutex<Option<Sender<i32>>> = Mutex::new(None);
}

#[ctor]
fn init() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    debug!("init called");
    let config = Config::init_from_env().unwrap();
    let pf = RUNTIME.block_on(pod_api::create_agent(
        &config.impersonated_pod_name,
        &config.impersonated_pod_namespace,
        &config.agent_namespace,
        config.agent_rust_log,
        config.agent_image,
    ));
    let (sender, receiver) = channel::<i32>(1000);
    *NEW_CONNECTION_SENDER.lock().unwrap() = Some(sender);
    enable_hooks();
    RUNTIME.spawn(poll_agent(pf, receiver));
}

async fn poll_agent(mut pf: Portforwarder, mut receiver: Receiver<i32>) {
    let port = pf.take_stream(61337).unwrap(); // TODO: Make port configurable
    let mut codec = actix_codec::Framed::new(port, ClientCodec::new());
    loop {
        select! {
            message = receiver.recv() => {
                match message {
                    Some(sockfd) => {
                        let port = SOCKETS.get_connection_socket_address(sockfd).unwrap().port();
                        debug!("send message to client {:?}", port);
                        codec.send(ClientMessage::PortSubscribe(vec![port])).await.unwrap();
                    }
                    None => {
                        debug!("NONE in recv");
                        break
                    }
                }
            }
            message = codec.next() => {
                match message {
                    Some(Ok(DaemonMessage::NewTCPConnection(conn))) => {
                        SOCKETS.open_connection(conn.connection_id, conn.port);
                    }
                    Some(Ok(DaemonMessage::TCPData(d))) => {
                        // Write to socket - need to find it in OPEN_CONNECTION_SOCKETS by conn_id
                        SOCKETS.write_data(d.connection_id, d.data);
                    }
                    Some(Ok(DaemonMessage::TCPClose(d))) => {
                        SOCKETS.close_connection(d.connection_id)
                    }
                    Some(_) => {
                        debug!("NONE in some");
                        break
                    },
                    None => {
                        thread::sleep(Duration::from_millis(2000));
                        debug!("NONE in none");
                        continue
                    }
                }
            }
        }
    }
}

fn enable_hooks() {
    let mut interceptor = Interceptor::obtain(&GUM);
    sockets::enable_hooks(interceptor)
}
