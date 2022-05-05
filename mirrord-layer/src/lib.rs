// #![feature(c_variadic)]

use std::{sync::Mutex, thread, time::Duration};

use ctor::ctor;
use frida_gum::{interceptor::Interceptor, Error, Gum, Module, NativePointer};
use futures::{SinkExt, StreamExt};
use kube::api::Portforwarder;
use lazy_static::lazy_static;
use libc::{c_char, c_void, sockaddr, socklen_t};
use mirrord_protocol::{ClientCodec, ClientMessage, DaemonMessage};
use os_socketaddr::OsSocketAddr;
use tokio::{
    runtime::Runtime,
    select,
    sync::mpsc::{channel, Receiver, Sender},
};
use tracing::{debug, error};

mod pod_api;
mod sockets;

lazy_static! {
    static ref GUM: Gum = unsafe { Gum::obtain() };
    static ref RUNTIME: Runtime = Runtime::new().unwrap();
    static ref SOCKETS: sockets::Sockets = sockets::Sockets::default();
    static ref NEW_CONNECTION_SENDER: Mutex<Option<Sender<i32>>> = Mutex::new(None);
}

#[ctor]
fn init() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();
    debug!("init called");

    let pf = RUNTIME.block_on(pod_api::create_agent());
    let (sender, receiver) = channel::<i32>(1000);
    *NEW_CONNECTION_SENDER.lock().unwrap() = Some(sender);
    enable_hooks();
    RUNTIME.spawn(poll_agent(pf, receiver));
}

unsafe extern "C" fn socket_detour(_domain: i32, _socket_type: i32, _protocol: i32) -> i32 {
    debug!("socket called");
    SOCKETS.create_socket()
}

unsafe extern "C" fn bind_detour(sockfd: i32, addr: *const sockaddr, addrlen: socklen_t) -> i32 {
    debug!("bind called");
    let parsed_addr = OsSocketAddr::from_raw_parts(addr as *const u8, addrlen as usize)
        .into_addr()
        .unwrap();

    SOCKETS.convert_to_connection_socket(sockfd, parsed_addr);
    0
}

unsafe extern "C" fn listen_detour(sockfd: i32, _backlog: i32) -> i32 {
    debug!("listen called");

    match SOCKETS.set_connection_state(sockfd, sockets::ConnectionState::Listening) {
        Ok(()) => {
            let sender = NEW_CONNECTION_SENDER.lock().unwrap();
            sender.as_ref().unwrap().blocking_send(sockfd).unwrap(); // Tell main thread to subscribe to agent
            0
        }
        Err(()) => {
            error!("Failed to set connection state to listening");
            -1
        }
    }
}

unsafe extern "C" fn getpeername_detour(
    sockfd: i32,
    addr: *mut sockaddr,
    addrlen: *mut socklen_t,
) -> i32 {
    let socket_addr = SOCKETS.get_data_socket_address(sockfd).unwrap();
    let os_addr: OsSocketAddr = socket_addr.into();
    let len = std::cmp::min(*addrlen as usize, os_addr.len() as usize);
    std::ptr::copy_nonoverlapping(os_addr.as_ptr() as *const u8, addr as *mut u8, len);

    *addrlen = os_addr.len();
    0
}

unsafe extern "C" fn setsockopt_detour(
    _sockfd: i32,
    _level: i32,
    _optname: i32,
    _optval: *mut c_char,
    _optlen: socklen_t,
) -> i32 {
    0
}

unsafe extern "C" fn accept_detour(
    sockfd: i32,
    addr: *mut sockaddr,
    addrlen: *mut socklen_t,
) -> i32 {
    debug!(
        "Accept called with sockfd {:?}, addr {:?}, addrlen {:?}",
        &sockfd, &addr, &addrlen
    );
    let socket_addr = SOCKETS.get_connection_socket_address(sockfd).unwrap();

    if !addr.is_null() {
        debug!("received non-null address in accept");
        let os_addr: OsSocketAddr = socket_addr.into();
        std::ptr::copy_nonoverlapping(os_addr.as_ptr(), addr, os_addr.len() as usize);
    }

    let connection_id = SOCKETS.read_single_connection(sockfd);
    SOCKETS.create_data_socket(connection_id, socket_addr)
}

unsafe extern "C" fn accept4_detour(
    sockfd: i32,
    addr: *mut sockaddr,
    addrlen: *mut socklen_t,
    _flags: i32,
) -> i32 {
    accept_detour(sockfd, addr, addrlen)
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

macro_rules! hook {
    ($interceptor:expr, $func:expr, $detour_name:expr) => {
        $interceptor
            .replace(
                Module::find_export_by_name(None, $func).unwrap(),
                NativePointer($detour_name as *mut c_void),
                NativePointer(std::ptr::null_mut::<c_void>()),
            )
            .unwrap();
    };
}

macro_rules! try_hook {
    ($interceptor:expr, $func:expr, $detour_name:expr) => {
        if let Some(addr) = Module::find_export_by_name(None, $func) {
            match $interceptor.replace(
                addr,
                NativePointer($detour_name as *mut c_void),
                NativePointer(std::ptr::null_mut::<c_void>()),
            ) {
                Err(Error::InterceptorAlreadyReplaced) => {
                    debug!("{} already replaced", $func);
                }
                Err(e) => {
                    debug!("{} error: {:?}", $func, e);
                }
                Ok(_) => {
                    debug!("{} hooked", $func);
                }
            }
        }
    };
}

fn enable_hooks() {
    let mut interceptor = Interceptor::obtain(&GUM);
    hook!(interceptor, "socket", socket_detour);
    hook!(interceptor, "bind", bind_detour);
    hook!(interceptor, "listen", listen_detour);
    hook!(interceptor, "getpeername", getpeername_detour);
    hook!(interceptor, "setsockopt", setsockopt_detour);
    try_hook!(interceptor, "uv__accept4", accept4_detour);
    try_hook!(interceptor, "accept4", accept4_detour);
    try_hook!(interceptor, "accept", accept_detour);
}
