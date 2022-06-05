#![feature(result_option_inspect)]
use actix_codec::{AsyncRead, AsyncWrite};
use ctor::ctor;
use envconfig::Envconfig;
use frida_gum::{interceptor::Interceptor, Gum};
use futures::{SinkExt, StreamExt};
use kube::api::Portforwarder;
use lazy_static::lazy_static;
use mirrord_protocol::{ClientCodec, ClientMessage, DaemonMessage};
use tokio::{
    runtime::Runtime,
    select,
    sync::mpsc::{channel, Receiver, Sender},
    time::{sleep, Duration},
};
use tracing::{debug, info, trace};

mod common;
mod config;
mod macros;
mod pod_api;
mod sockets;
mod tcp;
mod tcp_mirror;
use tracing_subscriber::prelude::*;

use crate::{
    common::HookMessage,
    config::Config,
    tcp::{create_tcp_handler, TCPApi, TCPHandler},
    tcp_mirror::TCPMirrorHandler,
};

lazy_static! {
    static ref GUM: Gum = unsafe { Gum::obtain() };
    static ref RUNTIME: Runtime = Runtime::new().unwrap();
}

pub static mut HOOK_SENDER: Option<Sender<HookMessage>> = None;

#[ctor]
fn init() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    info!("Initializing mirrord-layer!");

    let config = Config::init_from_env().unwrap();

    let pf = RUNTIME.block_on(pod_api::create_agent(
        &config.impersonated_pod_name,
        &config.impersonated_pod_namespace,
        &config.agent_namespace,
        config.agent_rust_log,
        config.agent_image.unwrap_or_else(|| {
            concat!("ghcr.io/metalbear-co/mirrord:", env!("CARGO_PKG_VERSION")).to_string()
        }),
    ));

    let (sender, receiver) = channel::<HookMessage>(1000);
    unsafe {
        HOOK_SENDER = Some(sender);
    };

    enable_hooks();

    RUNTIME.spawn(poll_agent(pf, receiver));
}

#[inline]
async fn handle_hook_message(
    hook_message: HookMessage,
    mirror_api: &mut TCPApi,
    codec: &mut actix_codec::Framed<impl AsyncRead + AsyncWrite + Unpin, ClientCodec>,
) {
    match hook_message {
        HookMessage::Listen(listen_message) => {
            debug!("HookMessage::Listen {:?}", listen_message);
            codec
                .send(ClientMessage::PortSubscribe(vec![listen_message.real_port]))
                .await
                .map(|()| async { mirror_api.listen_request(listen_message).await.unwrap() })
                .unwrap()
                .await;
        }
    }
}

#[inline]
async fn handle_daemon_message(
    daemon_message: DaemonMessage,
    mirror_api: &mut TCPApi,
    ping: &mut bool,
) {
    match daemon_message {
        DaemonMessage::NewTCPConnection(conn) => {
            debug!("DaemonMessage::NewTCPConnection {conn:#?}");
            mirror_api.new_tcp_connection(conn).await.unwrap();
        }
        DaemonMessage::TCPData(msg) => {
            mirror_api.tcp_data(msg).await.unwrap();
        }
        DaemonMessage::TCPClose(msg) => {
            mirror_api.tcp_close(msg).await.unwrap();
        }
        DaemonMessage::Pong => {
            if *ping {
                *ping = false;
                trace!("Daemon sent pong!");
            } else {
                panic!("Daemon: unmatched pong!");
            }
        }
        DaemonMessage::Close => todo!(),
        DaemonMessage::LogMessage(_) => todo!(),
    }
}

async fn poll_agent(mut pf: Portforwarder, mut receiver: Receiver<HookMessage>) {
    let port = pf.take_stream(61337).unwrap(); // TODO: Make port configurable

    // `codec` is used to retrieve messages from the daemon (messages that are sent from -agent to
    // -layer)
    let mut codec = actix_codec::Framed::new(port, ClientCodec::new());
    let mut ping = false;
    let (tcp_mirror_handler, mut mirror_api, config) = create_tcp_handler::<TCPMirrorHandler>();
    tokio::spawn(async move { tcp_mirror_handler.run(config).await });
    loop {
        select! {
            hook_message = receiver.recv() => {
                handle_hook_message(hook_message.unwrap(), &mut mirror_api, &mut codec).await;
            }
            daemon_message = codec.next() => {
                handle_daemon_message(daemon_message.unwrap().unwrap(),&mut mirror_api, &mut ping).await;
            }
            _ = sleep(Duration::from_secs(60)) => {
                if !ping {
                    codec.send(ClientMessage::Ping).await.unwrap();
                    trace!("sent ping to daemon");
                    ping = true;
                } else {
                    panic!("Client: unmatched ping");
                }
            }
        }
    }
}

fn enable_hooks() {
    let interceptor = Interceptor::obtain(&GUM);
    sockets::enable_hooks(interceptor)
}
