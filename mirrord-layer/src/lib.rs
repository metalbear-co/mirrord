// #![feature(c_variadic)]

use std::{
    mem::MaybeUninit,
    sync::{Mutex, Once},
    thread,
    time::Duration,
};

use ctor::ctor;
use frida_gum::{interceptor::Interceptor, Error, Gum, Module, NativePointer};
use futures::{SinkExt, StreamExt};
use kube::api::Portforwarder;
use lazy_static::lazy_static;
use libc::{c_char, c_int, c_void, sockaddr, socklen_t};
use mirrord_protocol::{ClientCodec, ClientMessage, DaemonMessage};
use os_socketaddr::OsSocketAddr;
use tokio::{
    runtime::Runtime,
    select,
    sync::mpsc::{channel, Receiver, Sender},
};
use tracing::{debug, error};
use tracing_subscriber::{prelude::__tracing_subscriber_SubscriberExt, util::SubscriberInitExt};

mod file;
mod pod_api;
mod sockets;

// TODO(alex) [high] 2022-05-09: After running for a while, it never displays anything (no response
// to curl) after "send message to client 7777". Later it starts to output "NONE in none".

lazy_static! {
    static ref GUM: Gum = unsafe { Gum::obtain() };
    static ref RUNTIME: Runtime = Runtime::new().unwrap();
    static ref SOCKETS: sockets::Sockets = sockets::Sockets::default();
    static ref NEW_CONNECTION_SENDER: Mutex<Option<Sender<i32>>> = Mutex::new(None);
}

static mut MAIN_FUNCTION: MaybeUninit<MainFn> = MaybeUninit::uninit();
static INIT_MAIN_FN: Once = Once::new();

type MainFn = extern "C" fn(c_int, *const *const c_char, *const *const c_char) -> c_int;
type LibcStartMainArgs = fn(
    MainFn,
    c_int,
    *const *const c_char,
    extern "C" fn(),
    extern "C" fn(),
    extern "C" fn(),
    extern "C" fn(),
) -> c_int;

#[ctor]
fn init() {
    let mut interceptor = Interceptor::obtain(&GUM);
    interceptor
        .replace(
            Module::find_export_by_name(None, "__libc_start_main").unwrap(),
            // TODO(alex) [low] 2022-05-03: Is there another way of converting a function into a
            // pointer?
            NativePointer(libc_start_main_detour as *mut c_void),
            NativePointer(std::ptr::null_mut()),
        )
        .unwrap();

    // tracing_subscriber::registry()
    //     .with(tracing_subscriber::fmt::layer())
    //     .with(tracing_subscriber::EnvFilter::from_default_env())
    //     .init();
    // debug!("init called");

    // let pf = RUNTIME.block_on(pod_api::create_agent());
    // let (sender, receiver) = channel::<i32>(1000);
    // *NEW_CONNECTION_SENDER.lock().unwrap() = Some(sender);
    // enable_hooks();
    // RUNTIME.spawn(poll_agent(pf, receiver));
}

// TODO(alex) [high] 2022-05-09: Calculate amount of files ignored if we do normal `init`, versus
// later initialization with `libc_start_main_detour`, see if they actually differ in the quantity
// of files loaded.
unsafe extern "C" fn libc_start_main_detour(
    main_fn: MainFn,
    argc: c_int,
    ubp_av: *const *const c_char,
    init: extern "C" fn(),
    fini: extern "C" fn(),
    rtld_fini: extern "C" fn(),
    stack_end: extern "C" fn(),
) -> i32 {
    debug!("loaded libc_start_main_detour");

    let mut interceptor = Interceptor::obtain(&GUM);
    let libc_start_main_ptr = Module::find_export_by_name(None, "__libc_start_main").unwrap();
    let real_libc_start_main: LibcStartMainArgs = std::mem::transmute(libc_start_main_ptr.0);
    // let libc_start_main_ptr = libc::dlsym(
    //     libc::RTLD_NEXT,
    //     CString::new("__libc_start_main").unwrap().into_raw(),
    // );
    // let real_libc_start_main: LibMainArgs = std::mem::transmute(libc_start_main_ptr);

    debug!("preparing the program's main function to be called later");
    INIT_MAIN_FN.call_once(|| {
        MAIN_FUNCTION = MaybeUninit::new(main_fn);
    });

    // real_main(main_fn, argc, ubp_av, init, fini, rtld_fini, stack_end)
    real_libc_start_main(main_detour, argc, ubp_av, init, fini, rtld_fini, stack_end)
}

// WARNING(alex): Normal `main` can't be found, so it doesn't work.
extern "C" fn main_detour(
    argc: c_int,
    argv: *const *const c_char,
    envp: *const *const c_char,
) -> c_int {
    unsafe {
        debug!("loaded main_detour");

        debug!("Hello from fake main!");

        tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer())
            .with(tracing_subscriber::EnvFilter::from_default_env())
            .init();
        debug!("init called");

        let pf = RUNTIME.block_on(pod_api::create_agent());
        let (sender, receiver) = channel::<i32>(1000);
        *NEW_CONNECTION_SENDER.lock().unwrap() = Some(sender);
        enable_hooks();
        RUNTIME.spawn(poll_agent(pf, receiver));

        let real_main = MAIN_FUNCTION.assume_init();

        debug!("about to call program's main");
        real_main(argc, argv, envp)
    }
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
    interceptor.begin_transaction();

    hook!(interceptor, "socket", socket_detour);
    hook!(interceptor, "bind", bind_detour);
    hook!(interceptor, "listen", listen_detour);
    hook!(interceptor, "getpeername", getpeername_detour);
    hook!(interceptor, "setsockopt", setsockopt_detour);
    try_hook!(interceptor, "uv__accept4", accept4_detour);
    try_hook!(interceptor, "accept4", accept4_detour);
    try_hook!(interceptor, "accept", accept_detour);

    file::enable_file_hooks(&mut interceptor);

    interceptor.end_transaction();
}
