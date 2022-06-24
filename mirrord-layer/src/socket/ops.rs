use std::{
    os::unix::{io::RawFd, prelude::*},
    sync::Arc,
};

use libc::{c_int, sockaddr, socklen_t};
use os_socketaddr::OsSocketAddr;
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use tracing::{debug, error, trace, warn};

use super::*;
use crate::{
    blocking_send_hook_message,
    error::LayerError,
    message::HookMessage,
    tcp::{HookMessageTcp, Listen},
};

/// Create the socket, add it to `MANAGED_SOCKETS` if it's successful, plus protocol and domain
/// matches our expected type (Tcpv4/v6).
///
/// Otherwise the socket is added to `BYPASS_SOCKETS`.
pub(super) fn socket(domain: c_int, type_: c_int, protocol: c_int) -> Result<RawFd, LayerError> {
    trace!(
        "socket -> domain {:#?} | type {:#?} | protocol {:#?}",
        domain,
        type_,
        protocol
    );

    let socket = Socket::new(
        Domain::from(domain),
        Type::from(type_),
        Some(Protocol::from(protocol)),
    )?;

    let fd = socket.as_raw_fd();

    // We don't handle non Tcpv4 sockets
    if !((domain == libc::AF_INET) || (domain == libc::AF_INET6) && (type_ & libc::SOCK_STREAM) > 0)
    {
        warn!(
            "socket -> Skipping non tcp socket domain {:#?} | type {:#?}",
            domain, type_
        );

        BYPASS_SOCKETS.lock().unwrap().insert(fd, socket);
    } else {
        let fake_socket = ManagedSocket {
            inner_socket: socket,
            state: SocketState::Initialized,
        };

        MANAGED_SOCKETS
            .lock()
            .unwrap()
            .insert(fd, Arc::new(fake_socket));
    }

    Ok(fd)
}

/// Binds the socket, no matter to which list it belongs (`MANAGED_SOCKETS`, or `BYPASS_SOCKETS`),
/// but if the `SocketAddr::port` matches our ignored port list (`is_ignored_port`), and this socket
/// is in `MANAGED_SOCKETS`, then this function will move the socket to `BYPASS_SOCKETS`.
pub(super) fn bind(sockfd: c_int, address: SocketAddr) -> Result<(), LayerError> {
    debug!("bind -> socket {:#?} | address {:#?}", sockfd, address);

    let bind_result = if let Some(socket) = MANAGED_SOCKETS
        .lock()
        .unwrap()
        .get(&sockfd)
        .map(|managed_socket| &managed_socket.inner_socket)
        .or_else(|| BYPASS_SOCKETS.lock().unwrap().get(&sockfd))
    {
        let address = if is_ignored_port(address.port()) {
            address
        } else {
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0)
        };

        Ok(socket.bind(&SockAddr::from(address))?)
    } else {
        Err(LayerError::LocalFDNotFound(sockfd))
    };

    // Put this socket into the bypassed list if it fails the `is_ignored_port` check, now that we
    // have more information about it (address).
    if let Ok(_) = bind_result {
        if is_ignored_port(address.port()) {
            warn!(
                "bind -> sockfd {:#?} has an ignored port {:#?}, moving it to bypass list!",
                sockfd,
                address.port()
            );

            let invalid_managed = MANAGED_SOCKETS.lock().unwrap().remove(&sockfd).unwrap();

            BYPASS_SOCKETS
                .lock()
                .unwrap()
                .insert(sockfd, invalid_managed.inner_socket);
        }
    }

    bind_result
}

/// Bind the socket to a fake, local port, and subscribe to the agent on the real port.
/// Messages received from the agent on the real port will later be routed to the fake local port.
pub(super) fn listen(
    socket: &mut ManagedSocket,
    bound: Bound,
    os_addr: OsSocketAddr,
    fd: RawFd,
) -> Result<(), LayerError> {
    debug!(
        "listen -> socket {:#?} | os_addr {:#?} | fd {:#?}",
        socket, os_addr, fd
    );

    let addr = os_addr
        .into_addr()
        .ok_or(LayerError::ParseSocketAddr(os_addr))?;

    blocking_send_hook_message(HookMessage::Tcp(HookMessageTcp::Listen(Listen {
        fake_port: addr.port(),
        real_port: bound.address.port(),
        ipv6: addr.is_ipv6(),
        fd,
    })))?;

    socket.state = SocketState::Listening(bound);
    debug!("listen: success");

    Ok(())
}

pub(super) fn connect(
    sockfd: RawFd,
    address: *const sockaddr,
    len: socklen_t,
) -> Result<(), LayerError> {
    todo!()
}

/// Resolve fake local address to real remote address. (IP & port of incoming traffic on the
/// cluster)
/// See https://github.com/rust-lang/rust-clippy/issues/8963
pub(super) fn getpeername(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> c_int {
    todo!()
}

/// Resolve the fake local address to the real local address.
#[allow(clippy::significant_drop_in_scrutinee)]
/// See https://github.com/rust-lang/rust-clippy/issues/8963
pub(super) fn getsockname(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> c_int {
    todo!()
}

/// When the fd is "ours", we accept and recv the first bytes that contain metadata on the
/// connection to be set in our lock This enables us to have a safe way to get "remote" information
/// (remote ip, port, etc).
pub(super) fn accept(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    new_fd: RawFd,
) -> RawFd {
    let (local_address, domain, protocol, type_) = {
        if let Some(socket) = MANAGED_SOCKETS.lock().unwrap().get(&sockfd) {
            if let SocketState::Listening(bound) = &socket.state {
                (bound.address, socket.domain, socket.protocol, socket.type_)
            } else {
                error!("original socket is not listening");
                return new_fd;
            }
        } else {
            debug!("origin socket not found");
            return new_fd;
        }
    };
    let socket_info = { CONNECTION_QUEUE.lock().unwrap().get(&sockfd) };
    let remote_address = match socket_info {
        Some(socket_info) => socket_info,
        None => {
            debug!("accept: socketinformation not found, probably not ours");
            return new_fd;
        }
    }
    .address;
    let new_socket = ManagedSocket {
        domain,
        protocol,
        type_,
        state: SocketState::Connected(Connected {
            remote_address,
            local_address,
        }),
    };
    fill_address(address, address_len, remote_address);

    MANAGED_SOCKETS
        .lock()
        .unwrap()
        .insert(new_fd, Arc::new(new_socket));
    new_fd
}

pub(super) fn fcntl(orig_fd: c_int, cmd: c_int, fcntl_fd: i32) -> c_int {
    if fcntl_fd == -1 {
        error!("fcntl failed");
        return fcntl_fd;
    }
    match cmd {
        libc::F_DUPFD | libc::F_DUPFD_CLOEXEC => {
            dup(orig_fd, fcntl_fd);
        }
        _ => (),
    }
    fcntl_fd
}

pub(super) fn dup(fd: c_int, dup_fd: i32) -> c_int {
    if dup_fd == -1 {
        error!("dup failed");
        return dup_fd;
    }
    let mut sockets = MANAGED_SOCKETS.lock().unwrap();
    if let Some(socket) = sockets.get(&fd) {
        let dup_socket = socket.clone();
        sockets.insert(dup_fd as RawFd, dup_socket);
    }
    dup_fd
}
