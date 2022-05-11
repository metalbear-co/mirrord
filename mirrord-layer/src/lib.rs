#![feature(once_cell)]

use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    thread,
    time::Duration,
};

use ctor::ctor;
use envconfig::Envconfig;
use frida_gum::{interceptor::Interceptor, Gum};
use futures::{SinkExt, StreamExt};
use kube::api::Portforwarder;
use lazy_static::lazy_static;
use libc::{c_char, c_int, c_void, sockaddr, socklen_t};
use mirrord_protocol::{ClientCodec, ClientMessage, DaemonMessage};
use tokio::{
    io::AsyncWriteExt,
    net::TcpStream,
    runtime::Runtime,
    select,
    sync::mpsc::{channel, Receiver, Sender},
    task,
};
use tracing::{debug, error};
use tracing_subscriber::{prelude::__tracing_subscriber_SubscriberExt, util::SubscriberInitExt};

mod common;
mod config;
mod file;
mod macros;
mod pod_api;
mod sockets;

use crate::{
    common::{HookMessage, Port},
    config::Config,
};

lazy_static! {
    static ref GUM: Gum = unsafe { Gum::obtain() };
    static ref RUNTIME: Runtime = Runtime::new().unwrap();
}

pub static mut HOOK_SENDER: Option<Sender<HookMessage>> = None;

#[derive(Debug)]
enum TcpTunnelMessages {
    Data(Vec<u8>),
    Close,
}

#[derive(Debug, Clone)]
struct ListenData {
    ipv6: bool,
    port: Port,
}

async fn tcp_tunnel(mut local_stream: TcpStream, mut receiver: Receiver<TcpTunnelMessages>) {
    loop {
        select! {
            message = receiver.recv() => {
                match message {
                    Some(TcpTunnelMessages::Data(data)) => {
                        local_stream.write_all(&data).await.unwrap()
                    },
                    Some(TcpTunnelMessages::Close) => break,
                    None => break
                };
            },
            _ = local_stream.readable() => {
                let mut data = vec![0; 1024];
                match local_stream.try_read(&mut data) {
                    Err(ref err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        continue
                        },
                    Err(err) => {
                        debug!("local stream ended with err {:?}", err);
                        break;
                    }
                    Ok(n) if n == 0 => break,
                    Ok(_) => {}
                }

            }
        }
    }
    debug!("exiting tcp tunnel");
}

#[ctor]
fn init() {
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    debug!("Initializing hooks from ctor!");

    let config = Config::init_from_env().unwrap();
    let pf = RUNTIME.block_on(pod_api::create_agent(
        &config.impersonated_pod_name,
        &config.impersonated_pod_namespace,
        &config.agent_namespace,
        config.agent_rust_log,
        config.agent_image,
    ));
    let (sender, receiver) = channel::<HookMessage>(1000);
    unsafe {
        HOOK_SENDER = Some(sender);
    };
    enable_hooks();

    RUNTIME.spawn(poll_agent(pf, receiver));
}

async fn poll_agent(mut pf: Portforwarder, mut receiver: Receiver<HookMessage>) {
    let port = pf.take_stream(61337).unwrap(); // TODO: Make port configurable
    let mut codec = actix_codec::Framed::new(port, ClientCodec::new());
    let mut port_mapping: HashMap<Port, ListenData> = HashMap::new();
    let mut active_connections = HashMap::new();
    loop {
        select! {
            message = receiver.recv() => {
                match message {
                    Some(HookMessage::Listen(msg)) => {
                        // let port = SOCKETS.get_connection_socket_address(sockfd).unwrap().port();
                        // debug!("send message to client {:?}", port);
                        debug!("received message from hook {:?}", msg);
                        codec.send(ClientMessage::PortSubscribe(vec![msg.real_port])).await.unwrap();
                        port_mapping.insert(msg.real_port, ListenData{port: msg.fake_port, ipv6: msg.ipv6});
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
                        debug!("new connection {:?}", conn);
                        let listen_data = match port_mapping.get(&conn.port) {
                            Some(listen_data) => (*listen_data).clone(),
                            None => {
                                debug!("no listen_data for {:?}", conn.port);
                                continue;
                            }
                        };
                        let addr = match listen_data.ipv6 {
                            false => SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), listen_data.port),
                            true => SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), listen_data.port),
                        };
                        let stream = match TcpStream::connect(addr).await {
                            Ok(stream) => stream,
                            Err(err) => {
                                error!("failed to connect to port {:?}", err);
                                continue;
                            }
                        };
                        let (sender, receiver) = channel::<TcpTunnelMessages>(1000);
                        active_connections.insert(conn.connection_id, sender);
                        task::spawn(async move {
                            tcp_tunnel(stream, receiver).await
                        });
                    }
                    Some(Ok(DaemonMessage::TCPData(msg))) => {
                        let sender = match active_connections.get(&msg.connection_id) {
                            Some(sender) => sender,
                            None => {
                                debug!("no sender for {:?}", msg.connection_id);
                                continue;
                            }
                        };
                        if let Err(err) = sender.send(TcpTunnelMessages::Data(msg.data)).await {
                                debug!("sender error {:?}", err);
                                active_connections.remove(&msg.connection_id);
                        }
                    },
                    Some(Ok(DaemonMessage::TCPClose(msg))) => {
                        let sender = match active_connections.remove(&msg.connection_id) {
                            Some(sender) => sender,
                            None => {
                                debug!("no sender for {:?}", msg.connection_id);
                                continue;
                            }
                        };
                        if let Err(err) = sender.send(TcpTunnelMessages::Close).await {
                                debug!("sender error {:?}", err);
                        }
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
    interceptor.begin_transaction();

    sockets::enable_socket_hooks(&mut interceptor);
    file::enable_file_hooks(&mut interceptor);

    interceptor.end_transaction();
}

// static mut MAIN_FUNCTION: MaybeUninit<MainFn> = MaybeUninit::uninit();
// static INIT_MAIN_FN: Once = Once::new();

// type MainFn = extern "C" fn(c_int, *const *const c_char, *const *const c_char) -> c_int;
// type LibcStartMainArgs = fn(
//     MainFn,
//     c_int,
//     *const *const c_char,
//     extern "C" fn(),
//     extern "C" fn(),
//     extern "C" fn(),
//     extern "C" fn(),
// ) -> c_int;

// unsafe extern "C" fn libc_start_main_detour(
//     main_fn: MainFn,
//     argc: c_int,
//     ubp_av: *const *const c_char,
//     init: extern "C" fn(),
//     fini: extern "C" fn(),
//     rtld_fini: extern "C" fn(),
//     stack_end: extern "C" fn(),
// ) -> i32 {
//     debug!("loaded libc_start_main_detour");

//     let libc_start_main_ptr = Module::find_export_by_name(None, "__libc_start_main").unwrap();
//     let real_libc_start_main: LibcStartMainArgs = std::mem::transmute(libc_start_main_ptr.0);

//     debug!("preparing the program's main function to be called later");
//     INIT_MAIN_FN.call_once(|| {
//         MAIN_FUNCTION = MaybeUninit::new(main_fn);
//     });

//     // real_main(main_fn, argc, ubp_av, init, fini, rtld_fini, stack_end)
//     real_libc_start_main(main_detour, argc, ubp_av, init, fini, rtld_fini, stack_end)
// }

// WARNING(alex): Normal `main` can't be found, so it doesn't work.
// extern "C" fn main_detour(
//     argc: c_int,
//     argv: *const *const c_char,
//     envp: *const *const c_char,
// ) -> c_int {
//     unsafe {
//         debug!("loaded main_detour");

//         debug!("Hello from fake main!");

//         tracing_subscriber::registry()
//             .with(tracing_subscriber::fmt::layer())
//             .with(tracing_subscriber::EnvFilter::from_default_env())
//             .init();
//         debug!("init called");

//         let config = Config::init_from_env().unwrap();
//         let pf = RUNTIME.block_on(pod_api::create_agent(
//             &config.impersonated_pod_name,
//             &config.impersonated_pod_namespace,
//             &config.agent_namespace,
//             config.agent_rust_log,
//             config.agent_image,
//         ));
//         let (sender, receiver) = channel::<i32>(1000);
//         *NEW_CONNECTION_SENDER.lock().unwrap() = Some(sender);
//         enable_hooks();
//         RUNTIME.spawn(poll_agent(pf, receiver));

//         let real_main = MAIN_FUNCTION.assume_init();

//         debug!("about to call program's main");
//         real_main(argc, argv, envp)
//     }
// }
