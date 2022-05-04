#![feature(c_variadic)]

use std::{
    collections::HashMap,
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    os::unix::io::AsRawFd,
    sync::Mutex,
    thread,
    time::Duration,
};

use ctor::ctor;
use frida_gum::{interceptor::Interceptor, Gum, Module, NativePointer};
use futures::{SinkExt, StreamExt};
use k8s_openapi::api::core::v1::Pod;
use kube::{
    api::{Api, Portforwarder, PostParams},
    runtime::wait::{await_condition, conditions::is_pod_running},
    Client,
};
use lazy_static::lazy_static;
use libc::{c_char, c_void, sockaddr, socklen_t};
use mirrord_protocol::{ClientCodec, ClientMessage, DaemonMessage};
use multi_map::MultiMap;
use os_socketaddr::OsSocketAddr;
use queues::{IsQueue, Queue};
use rand::distributions::{Alphanumeric, DistString};
use serde_json::json;
use socketpair::{socketpair_stream, SocketpairStream};
use tokio::{
    runtime::Runtime,
    select,
    sync::mpsc::{channel, Receiver, Sender},
};
use tracing::{debug, error};

struct ConnectionSocket {
    read_fd: i32,
    read_socket: SocketpairStream,
    real_read_socket_fd: i32,
    write_socket: SocketpairStream,
    port: u16,
}

struct DataSocket {
    connection_id: u16,
    #[allow(dead_code)]
    read_socket: SocketpairStream, // Though this is unread, it's necessary to keep the socket open
    write_socket: SocketpairStream,
}

lazy_static! {
    static ref GUM: Gum = unsafe { Gum::obtain() };
    static ref RUNTIME: Runtime = Runtime::new().unwrap();
    static ref CONNECTION_SOCKETS: Mutex<MultiMap<i32, u16, ConnectionSocket>> =
        Mutex::new(MultiMap::new());
    static ref DATA_SOCKETS_BY_CONNECTION_ID: Mutex<HashMap<u16, DataSocket>> = Mutex::new(HashMap::new());
    static ref CONNECTION_QUEUE: Mutex<Queue<u16>> = Mutex::new(Queue::new());
    static ref SENDER: Mutex<Option<Sender<u16>>> = Mutex::new(None); // TODO: Check OnceCell
    static ref PENDING_DATA: Mutex<HashMap<u16, Vec<u8>>> = Mutex::new(HashMap::new());
}

unsafe extern "C" fn socket_detour(domain: i32, socket_type: i32, protocol: i32) -> i32 {
    debug!("socket called");
    let (write_socket, read_socket) = socketpair_stream().unwrap();
    let read_fd = read_socket.as_raw_fd();
    let socket = ConnectionSocket {
        read_fd,
        read_socket,
        real_read_socket_fd: libc::socket(domain, socket_type, protocol),
        write_socket,
        port: 0,
    };

    CONNECTION_SOCKETS
        .lock()
        .unwrap()
        .insert(read_fd, 0, socket);

    read_fd
}

unsafe extern "C" fn bind_detour(sockfd: i32, addr: *const sockaddr, addrlen: socklen_t) -> i32 {
    debug!("bind called");
    let parsed_addr = OsSocketAddr::from_raw_parts(addr as *const u8, addrlen as usize)
        .into_addr()
        .unwrap();
    let port = parsed_addr.port();

    let mut sockets = CONNECTION_SOCKETS.lock().unwrap();
    if let Some(mut socket) = sockets.remove(&sockfd) {
        socket.port = port;
        sockets.insert(sockfd, port, socket);

        0
    } else {
        error!("No socket found for fd: {}", sockfd);
        -1
    }
}

unsafe extern "C" fn connect_detour(sockfd: i32, addr: *const sockaddr, addrlen: socklen_t) -> i32 {
    // Map port to fd
    debug!("Called connect");
    let real_fd = match CONNECTION_SOCKETS.lock().unwrap().get(&sockfd) {
        Some(s) => s.real_read_socket_fd,
        None => {
            println!("No socket found for fd: {}", sockfd);
            return -1;
        }
    };

    libc::connect(real_fd, addr, addrlen)
}

unsafe extern "C" fn listen_detour(sockfd: i32, _backlog: i32) -> i32 {
    debug!("listen called");

    let mut sockets = CONNECTION_SOCKETS.lock().unwrap();

    if let Some(socket) = sockets.remove(&sockfd) {
        let lock = SENDER.lock().unwrap();
        let send = lock.as_ref();
        send.unwrap().blocking_send(socket.port).unwrap();
        sockets.insert(sockfd, socket.port, socket);
        0
    } else {
        error!("No socket found for fd: {}", sockfd);
        -1
    }
}

unsafe extern "C" fn getpeername_detour(
    _sockfd: i32,
    addr: *mut sockaddr,
    addrlen: *mut socklen_t,
) -> i32 {
    let socket_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
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

    if !addr.is_null() {
        debug!("received non-null address in accept");
        let socket_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
        let os_addr: OsSocketAddr = socket_addr.into();
        std::ptr::copy_nonoverlapping(os_addr.as_ptr(), addr, os_addr.len() as usize);
    }
    let connection_id;
    let mut sockets = CONNECTION_SOCKETS.lock().unwrap();

    if let Some(mut socket) = sockets.remove(&sockfd) {
        let mut buffer = [0; 1];
        socket.read_socket.read_exact(&mut buffer).unwrap();
        connection_id = CONNECTION_QUEUE.lock().unwrap().remove().unwrap();
        sockets.insert(sockfd, socket.port, socket);
    } else {
        error!("No socket found for fd: {}", sockfd);
        return -1;
    }

    let (read_socket, mut write_socket) = socketpair_stream().unwrap();
    let read_fd = read_socket.as_raw_fd();
    debug!(
        "Accepted connection from read_fd:{:?}, write_sock:{:?}",
        read_fd, write_socket
    );

    if let Some(data) = PENDING_DATA.lock().unwrap().remove(&connection_id) {
        debug!("writing pending data for connection_id: {}", connection_id);
        write_socket.write_all(&data).unwrap();
    }

    let data_socket = DataSocket {
        connection_id,
        read_socket,
        write_socket,
    };
    DATA_SOCKETS_BY_CONNECTION_ID
        .lock()
        .unwrap()
        .insert(connection_id, data_socket);

    read_fd
}

async fn create_agent() -> Portforwarder {
    // Create Agent
    let client = Client::try_default().await.unwrap();
    let pods: Api<Pod> = Api::namespaced(client, "default");
    let agent_pod_name = format!(
        "mirrord-agent-{}",
        Alphanumeric
            .sample_string(&mut rand::thread_rng(), 10)
            .to_lowercase()
    );
    let debug_pod: Pod = serde_json::from_value(
        json!({ // TODO: Make nodename, image configurable, container-id
            "metadata": {
                "name": agent_pod_name
            },
            "spec": {
                "hostPID": true,
                "nodeName": "aks-agentpool-11071180-vmss000000",
                "restartPolicy": "Never",
                "volumes": [
                    {
                        "name": "containerd",
                        "hostPath": {
                            "path": "/run/containerd/containerd.sock"
                        }
                    }
                ],
                "containers": [
                    {
                        "name": "mirrord-agent",
                        "image": "ghcr.io/metalbear-co/mirrord-agent:2.0.0-alpha-3",
                        "imagePullPolicy": "Always",
                        "securityContext": {
                            "privileged": true
                        },
                        "volumeMounts": [
                            {
                                "mountPath": "/run/containerd/containerd.sock",
                                "name": "containerd"
                            }
                        ],
                        "command": [
                            "./mirrord-agent",
                            "--container-id",
                            "af14a5800124573a93d17d9302a57bdda320d15bfdebd1995e6b1cc3fdb4fee7",
                            "-t",
                            "60"
                        ],
                        "env": [{"name": "RUST_LOG", "value": "trace"}],
                    }
                ]
            }
        }),
    )
    .unwrap();
    pods.create(&PostParams::default(), &debug_pod)
        .await
        .unwrap();

    //   Wait until the pod is running, otherwise we get 500 error.
    let running = await_condition(pods.clone(), &agent_pod_name, is_pod_running());
    let _ = tokio::time::timeout(std::time::Duration::from_secs(15), running)
        .await
        .unwrap();
    let pf = pods.portforward(&agent_pod_name, &[61337]).await.unwrap();

    pf
}

async fn poll_agent(mut pf: Portforwarder, mut receiver: Receiver<u16>) {
    let port = pf.take_stream(61337).unwrap(); // TODO: Make port configurable
    let mut codec = actix_codec::Framed::new(port, ClientCodec::new());
    thread::sleep(Duration::from_millis(5000));
    loop {
        select! {
            message = receiver.recv() => {
                match message {
                    Some(message) => {
                        debug!("send message to client {:?}", &message);
                        codec.send(ClientMessage::PortSubscribe(vec![message])).await.unwrap();
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
                        let mut sockets = CONNECTION_SOCKETS.lock().unwrap();
                        if let Some(mut socket) = sockets.remove_alt(&conn.port) {
                                debug!("new connection id: {:?}", &conn.connection_id);
                                write!(socket.write_socket, "a").unwrap();
                                CONNECTION_QUEUE.lock().unwrap().add(conn.connection_id).unwrap();
                                sockets.insert(socket.read_fd, socket.port, socket);
                            }
                            else
                            {
                                error!("No socket found for port: {}", conn.port);
                                break
                            }
                    }
                    Some(Ok(DaemonMessage::TCPData(d))) => {
                        // Write to socket - need to find it in OPEN_CONNECTION_SOCKETS by conn_id
                        let mut sockets = DATA_SOCKETS_BY_CONNECTION_ID.lock().unwrap();
                        if let Some(mut socket) = sockets.remove(&d.connection_id) {
                                socket.write_socket.write_all(&d.data).unwrap();
                                sockets.insert(socket.connection_id, socket);
                            }
                            else
                            {
                                // Not necessarily an error - sometime the TCPData message is handled before NewTcpConnection
                                debug!("No socket found for connection_id: {}", d.connection_id);
                                PENDING_DATA.lock().unwrap().insert(d.connection_id, d.data);
                                continue
                            }
                    }
                    Some(Ok(DaemonMessage::TCPClose(d))) => {
                        DATA_SOCKETS_BY_CONNECTION_ID.lock().unwrap().remove(&d.connection_id).unwrap();
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

#[ctor]
fn init() {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();
    debug!("init called");

    let pf = RUNTIME.block_on(create_agent());
    let (sender, receiver) = channel::<u16>(1000);
    *SENDER.lock().unwrap() = Some(sender);
    enable_hooks();
    RUNTIME.spawn(poll_agent(pf, receiver));
}

fn enable_hooks() {
    let mut interceptor = Interceptor::obtain(&GUM);

    interceptor
        .replace(
            Module::find_export_by_name(None, "socket").unwrap(),
            NativePointer(socket_detour as *mut c_void),
            NativePointer(std::ptr::null_mut::<c_void>()),
        )
        .unwrap();

    interceptor
        .replace(
            Module::find_export_by_name(None, "bind").unwrap(),
            NativePointer(bind_detour as *mut c_void),
            NativePointer(std::ptr::null_mut::<c_void>()),
        )
        .unwrap();

    interceptor
        .replace(
            Module::find_export_by_name(None, "connect").unwrap(),
            NativePointer(connect_detour as *mut c_void),
            NativePointer(std::ptr::null_mut::<c_void>()),
        )
        .unwrap();

    interceptor
        .replace(
            Module::find_export_by_name(None, "listen").unwrap(),
            NativePointer(listen_detour as *mut c_void),
            NativePointer(std::ptr::null_mut::<c_void>()),
        )
        .unwrap();

    interceptor
        .replace(
            Module::find_export_by_name(None, "getpeername").unwrap(),
            NativePointer(getpeername_detour as *mut c_void),
            NativePointer(std::ptr::null_mut::<c_void>()),
        )
        .unwrap();

    interceptor
        .replace(
            Module::find_export_by_name(None, "setsockopt").unwrap(),
            NativePointer(setsockopt_detour as *mut c_void),
            NativePointer(std::ptr::null_mut::<c_void>()),
        )
        .unwrap();

    interceptor
        .replace(
            Module::find_export_by_name(None, "accept").unwrap(),
            NativePointer(accept_detour as *mut c_void),
            NativePointer(std::ptr::null_mut::<c_void>()),
        )
        .unwrap();
}
