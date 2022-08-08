use std::{
    ffi::CString,
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener},
    os::unix::io::RawFd,
    ptr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use dns_lookup::AddrInfo;
use libc::{c_int, sockaddr, socklen_t};
use mirrord_protocol::ConnectResponse;
use socket2::{Domain, SockAddr};
use tokio::sync::oneshot;
use tracing::{debug, error, trace};

use super::{hooks::*, *};
use crate::{
    common::{blocking_send_hook_message, GetAddrInfoHook, HookMessage},
    error::LayerError,
    tcp::{
        outgoing::{Connect, CreateMirrorStream, MirrorConnect, OutgoingTraffic},
        HookMessageTcp, Listen,
    },
};

// TODO(alex) [high] 2022-07-21: Separate sockets into 2 compartments, listen sockets go into a
// thread for listening, connect sockets into another thread. Try to get rid of the overall global
// theme around sockets.
// Worth it?

pub(crate) static IS_INTERNAL_CALL: AtomicBool = AtomicBool::new(false);

/// Create the socket, add it to SOCKETS if successful and matching protocol and domain (Tcpv4/v6)
pub(super) fn socket(
    sockfd: RawFd,
    domain: c_int,
    type_: c_int,
    protocol: c_int,
) -> Result<RawFd, LayerError> {
    trace!("socket -> domain {:#?} | type {:#x?}", domain, type_);

    if !((domain == libc::AF_INET) || (domain == libc::AF_INET6) && (type_ & libc::SOCK_STREAM) > 0)
    {
        debug!("non Tcp socket domain:{:?}, type:{:?}", domain, type_);
    } else {
        let mut sockets = SOCKETS.lock()?;
        sockets.insert(
            sockfd,
            Arc::new(MirrorSocket {
                domain,
                type_,
                protocol,
                state: SocketState::default(),
            }),
        );
    }

    Ok(sockfd)
}

/// Check if the socket is managed by us, if it's managed by us and it's not an ignored port,
/// update the socket state and don't call bind (will be called later). In any other case, we call
/// regular bind.
pub(super) fn bind(sockfd: c_int, address: SocketAddr) -> Result<(), LayerError> {
    trace!("bind -> sockfd {:#?} | address {:#?}", sockfd, address);

    let mut socket = {
        SOCKETS
            .lock()?
            .remove(&sockfd)
            .ok_or(LayerError::LocalFDNotFound(sockfd))
            .and_then(|socket| {
                if !matches!(socket.state, SocketState::Initialized) {
                    Err(LayerError::SocketInvalidState(sockfd))
                } else {
                    Ok(socket)
                }
            })?
    };

    (!is_ignored_port(address.port()))
        .then_some(())
        .ok_or_else(|| LayerError::BypassedPort(address.port()))?;

    Arc::get_mut(&mut socket).unwrap().state = SocketState::Bound(Bound { address });

    SOCKETS.lock()?.insert(sockfd, socket);

    Ok(())
}

/// Bind the socket to a fake, local port, and subscribe to the agent on the real port.
/// Messages received from the agent on the real port will later be routed to the fake local port.
pub(super) fn listen(sockfd: RawFd, backlog: c_int) -> Result<(), LayerError> {
    debug!("listen -> sockfd {:#?} | backlog {:#?}", sockfd, backlog);

    let mut socket = {
        SOCKETS
            .lock()?
            .remove(&sockfd)
            .ok_or(LayerError::LocalFDNotFound(sockfd))?
    };

    match &socket.state {
        SocketState::Bound(bound) => {
            let real_port = bound.address.port();

            Arc::get_mut(&mut socket).unwrap().state = SocketState::Listening(*bound);

            let address = match socket.domain {
                libc::AF_INET => Ok(SockAddr::from(SocketAddr::new(
                    IpAddr::V4(Ipv4Addr::LOCALHOST),
                    0,
                ))),
                libc::AF_INET6 => Ok(SockAddr::from(SocketAddr::new(
                    IpAddr::V6(Ipv6Addr::UNSPECIFIED),
                    0,
                ))),
                invalid => Err(LayerError::UnsupportedDomain(invalid)),
            }?;

            let bind_result = unsafe { FN_BIND(sockfd, address.as_ptr(), address.len()) };
            if bind_result != 0 {
                error!(
                    "listen -> Failed `bind` sockfd {:#?} to address {:#?} with errno {:#?}!",
                    sockfd,
                    address,
                    errno::errno()
                );
                return Err(io::Error::from_raw_os_error(bind_result).into());
            }

            // We need to find out what's the port we bound to, that'll be used by `poll_agent` to
            // connect to.
            let getsockname_result =
                unsafe { FN_GETSOCKNAME(sockfd, address.as_ptr() as *mut _, &mut address.len()) };
            if getsockname_result != 0 {
                error!("listen -> Failed `getsockname` sockfd {:#?}", sockfd);

                return Err(io::Error::from_raw_os_error(getsockname_result).into());
            }

            let address = address.as_socket().ok_or(LayerError::AddressConversion);
            debug!("listen -> address {:#?}", address);

            let address = address?;

            let listen_result = unsafe { FN_LISTEN(sockfd, backlog) };
            if listen_result != 0 {
                error!("listen -> Failed `listen` sockfd {:#?}", sockfd);

                return Err(io::Error::from_raw_os_error(listen_result).into());
            }

            blocking_send_hook_message(HookMessage::Tcp(HookMessageTcp::Listen(Listen {
                fake_port: address.port(),
                real_port,
                ipv6: address.is_ipv6(),
                fd: sockfd,
            })))?;

            Ok(())
        }
        _ => Err(LayerError::SocketInvalidState(sockfd)),
    }?;

    SOCKETS.lock()?.insert(sockfd, socket);

    Ok(())
}

// TODO(alex) [high] 2022-08-05: So the flow for a `socket.connect` call in node is:
// `socket_detour` -> `socket` -> `bind_detour` -> `bind` -> `getsockname_detour` ->
// `getsockname` (0.0.0.0:8888, local) -> `connect_detour` -> `connect` (20.81.111.66:80, remote)
// -> crash.
//
// This means that there is some handling to be done in `bind` that must happen, and currently we're
// not doing it (probably the true `bind` call). Basically, this local socket is "fakely" created,
// but we need it truly bound and ready.
pub(super) fn connect(sockfd: RawFd, remote_address: SocketAddr) -> Result<(), LayerError> {
    trace!(
        "connect -> sockfd {:#?} | remote_address {:#?}",
        sockfd,
        remote_address
    );

    let mut user_socket_info = {
        SOCKETS
            .lock()?
            .remove(&sockfd)
            .ok_or(LayerError::LocalFDNotFound(sockfd))?
    };
    debug!("connect -> user_socket_info {:#?}", user_socket_info);

    if let SocketState::Initialized = user_socket_info.state {
        trace!(
            "connect -> SocketState::Initialized {:#?}",
            user_socket_info
        );
        // TODO(alex) [high] 2022-07-15: Implementation plan
        // 4. Calls to read on this socket in `agent` will contain data that we log, and then
        // write to `remote_address`;

        let (mirror_tx, mirror_rx) = oneshot::channel();

        let connect = Connect {
            user_fd: sockfd,
            remote_address,
            channel_tx: mirror_tx,
        };

        debug!("connect -> connect {:#?}", connect);

        let connect_hook = OutgoingTraffic::Connect(connect);

        blocking_send_hook_message(HookMessage::OutgoingTraffic(connect_hook))?;
        let MirrorConnect { mirror_address } = mirror_rx.blocking_recv()??;

        let connect_to = SockAddr::from(mirror_address);
        trace!("connect -> connect_to {:#?}", connect_to);

        let connect_result = unsafe { FN_CONNECT(sockfd, connect_to.as_ptr(), connect_to.len()) };
        if connect_result == -1 {
            return Err(todo!());
        }

        let connected = Connected {
            remote_address,
            mirror_address,
        };

        trace!("connect -> connected {:#?}", connected);

        Arc::get_mut(&mut user_socket_info).unwrap().state = SocketState::Connected(connected);

        Ok::<(), LayerError>(())
    } else if let SocketState::Bound(bound) = user_socket_info.state {
        {
            trace!("connect -> SocketState::Bound {:#?}", user_socket_info);

            let (mirror_tx, mirror_rx) = oneshot::channel();

            let connect = Connect {
                user_fd: sockfd,
                remote_address,
                channel_tx: mirror_tx,
            };

            debug!("connect -> connect {:#?}", connect);

            let connect_hook = OutgoingTraffic::Connect(connect);

            blocking_send_hook_message(HookMessage::OutgoingTraffic(connect_hook))?;
            let MirrorConnect { mirror_address } = mirror_rx.blocking_recv()??;

            let connect_to = SockAddr::from(mirror_address);
            trace!("connect -> connect_to {:#?}", connect_to);

            let connect_result =
                unsafe { FN_CONNECT(sockfd, connect_to.as_ptr(), connect_to.len()) };
            if connect_result == -1 {
                return Err(todo!());
            }

            let connected = Connected {
                remote_address,
                mirror_address,
            };

            trace!("connect -> connected {:#?}", connected);

            Arc::get_mut(&mut user_socket_info).unwrap().state = SocketState::Connected(connected);

            Ok::<(), LayerError>(())
        }

        // trace!("connect -> SocketState::Bound {:#?}", user_socket_info);

        // let address = SockAddr::from(bound.address);
        // let bind_result = unsafe { FN_BIND(sockfd, address.as_ptr(), address.len()) };

        // debug!("connect -> bind_result {:#?}", bind_result);

        // if bind_result != 0 {
        //     error!(
        //         "connect -> Failed to bind socket result {:?}, address: {:?}, sockfd: {:?}!",
        //         bind_result, address, sockfd
        //     );

        //     Err(io::Error::from_raw_os_error(bind_result))?
        // } else {
        //     let rawish_remote_address = SockAddr::from(remote_address);
        //     let result = unsafe {
        //         FN_CONNECT(
        //             sockfd,
        //             rawish_remote_address.as_ptr(),
        //             rawish_remote_address.len(),
        //         )
        //     };

        //     // TODO(alex) [high] 2022-08-05: Alright, here is the problem.
        //     // There is a `bind` call on the new local socket that'll connect to the remote
        // address,     // so we end up entering here, and not on the proper "connect
        // intercept" path.     if result != 0 {
        //         error!(
        //             "connect -> Failed to connect socket result {:?}, address: {:?}, sockfd:
        // {:?}!",             result, address, sockfd
        //         );

        //         Err(io::Error::from_raw_os_error(result))?
        //     } else {
        //         Ok::<_, LayerError>(())
        //     }
        // }
    } else {
        Err(LayerError::SocketInvalidState(sockfd))
    }?;

    Ok(())
}

/// Resolve fake local address to real remote address. (IP & port of incoming traffic on the
/// cluster)
pub(super) fn getpeername(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> Result<(), LayerError> {
    trace!("getpeername -> sockfd {:#?}", sockfd);

    let remote_address = {
        SOCKETS
            .lock()?
            .get(&sockfd)
            .ok_or(LayerError::LocalFDNotFound(sockfd))
            .and_then(|socket| match &socket.state {
                SocketState::Connected(connected) => Ok(connected.remote_address),
                _ => Err(LayerError::SocketInvalidState(sockfd)),
            })?
    };

    debug!("getpeername -> remote_address {:#?}", remote_address);

    fill_address(address, address_len, remote_address)
}
/// Resolve the fake local address to the real local address.
#[allow(clippy::significant_drop_in_scrutinee)]
/// See https://github.com/rust-lang/rust-clippy/issues/8963
pub(super) fn getsockname(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> Result<(), LayerError> {
    trace!("getsockname -> sockfd {:#?}", sockfd);

    let local_address = {
        SOCKETS
            .lock()?
            .get(&sockfd)
            .ok_or(LayerError::LocalFDNotFound(sockfd))
            .and_then(|socket| match &socket.state {
                SocketState::Connected(connected) => Ok(connected.mirror_address),
                SocketState::Bound(bound) => Ok(bound.address),
                SocketState::Listening(bound) => Ok(bound.address),
                _ => Err(LayerError::SocketInvalidState(sockfd)),
            })?
    };

    debug!("getsockname -> local_address {:#?}", local_address);

    fill_address(address, address_len, local_address)
}

/// When the fd is "ours", we accept and recv the first bytes that contain metadata on the
/// connection to be set in our lock This enables us to have a safe way to get "remote" information
/// (remote ip, port, etc).
pub(super) fn accept(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    new_fd: RawFd,
) -> Result<RawFd, LayerError> {
    let (local_address, domain, protocol, type_) = {
        SOCKETS
            .lock()?
            .get(&sockfd)
            .ok_or(LayerError::LocalFDNotFound(sockfd))
            .and_then(|socket| match &socket.state {
                SocketState::Listening(bound) => {
                    Ok((bound.address, socket.domain, socket.protocol, socket.type_))
                }
                _ => Err(LayerError::SocketInvalidState(sockfd)),
            })?
    };

    let remote_address = {
        CONNECTION_QUEUE
            .lock()?
            .get(&sockfd)
            .ok_or(LayerError::LocalFDNotFound(sockfd))
            .map(|socket| socket.address)?
    };

    let new_socket = MirrorSocket {
        domain,
        protocol,
        type_,
        state: SocketState::Connected(Connected {
            remote_address,
            mirror_address: local_address,
        }),
    };
    fill_address(address, address_len, remote_address)?;

    SOCKETS.lock()?.insert(new_fd, Arc::new(new_socket));

    Ok(new_fd)
}

pub(super) fn fcntl(orig_fd: c_int, cmd: c_int, fcntl_fd: i32) -> Result<(), LayerError> {
    match cmd {
        libc::F_DUPFD | libc::F_DUPFD_CLOEXEC => dup(orig_fd, fcntl_fd),
        _ => Ok(()),
    }
}

pub(super) fn dup(fd: c_int, dup_fd: i32) -> Result<(), LayerError> {
    let dup_socket = SOCKETS
        .lock()?
        .get(&fd)
        .ok_or(LayerError::LocalFDNotFound(fd))?
        .clone();

    SOCKETS.lock()?.insert(dup_fd as RawFd, dup_socket);

    Ok(())
}

/// Retrieves the result of calling `getaddrinfo` from a remote host (resolves remote DNS),
/// converting the result into a `Box` allocated raw pointer of `libc::addrinfo` (which is basically
/// a linked list of such type).
///
/// Even though individual parts of the received list may contain an error, this function will
/// still work fine, as it filters out such errors and returns a null pointer in this case.
///
/// # Protocol
///
/// `-layer` sends a request to `-agent` asking for the `-agent`'s list of `addrinfo`s (remote call
/// for the equivalent of this function).
pub(super) fn getaddrinfo(
    node: Option<String>,
    service: Option<String>,
    hints: Option<AddrInfoHint>,
) -> Result<*mut libc::addrinfo, LayerError> {
    trace!(
        "getaddrinfo -> node {:#?} | service {:#?} | hints {:#?}",
        node,
        service,
        hints
    );

    let (hook_channel_tx, hook_channel_rx) = oneshot::channel();
    let hook = GetAddrInfoHook {
        node,
        service,
        hints,
        hook_channel_tx,
    };

    blocking_send_hook_message(HookMessage::GetAddrInfoHook(hook))?;

    let addr_info_list = hook_channel_rx.blocking_recv()??;

    addr_info_list
        .into_iter()
        .map(AddrInfo::from)
        .map(|addr_info| {
            let AddrInfo {
                socktype,
                protocol,
                address,
                sockaddr,
                canonname,
                flags,
            } = addr_info;

            let sockaddr = socket2::SockAddr::from(sockaddr);

            let canonname = canonname.map(CString::new).transpose().unwrap();
            let ai_canonname = canonname.map_or_else(ptr::null, |c_string| {
                let c_str = c_string.as_c_str();
                c_str.as_ptr()
            }) as *mut _;

            libc::addrinfo {
                ai_flags: flags,
                ai_family: address,
                ai_socktype: socktype,
                ai_protocol: protocol,
                ai_addrlen: sockaddr.len(),
                ai_addr: sockaddr.as_ptr() as *mut _,
                ai_canonname,
                ai_next: ptr::null_mut(),
            }
        })
        .rev()
        .map(Box::new)
        .map(Box::into_raw)
        .reduce(|current, mut previous| {
            // Safety: These pointers were just allocated using `Box::new`, so they should be fine
            // regarding memory layout, and are not dangling.
            unsafe { (*previous).ai_next = current };
            previous
        })
        .ok_or(LayerError::DNSNoName)
}
