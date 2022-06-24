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
    HOOK_SENDER,
};

/// Create the socket, add it to `MIRROR_SOCKETS` if it's successful, plus protocol and domain
/// matches our expected type (Tcpv4/v6).
///
/// Otherwise the socket is added to `BYPASS_SOCKETS`.
pub(super) fn socket(domain: c_int, type_: c_int, protocol: c_int) -> Result<RawFd, LayerError> {
    debug!(
        "socket -> domain {:#?} | type {:#?} | protocol {:#?}",
        domain, type_, protocol
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
        let mirror_socket = MirrorSocket {
            inner: socket,
            state: SocketState::Initialized,
        };

        MIRROR_SOCKETS.lock().unwrap().insert(fd, mirror_socket);
    }

    Ok(fd)
}

/// Binds the socket, no matter to which list it belongs (`MIRROR_SOCKETS`, or `BYPASS_SOCKETS`),
/// but if the `SocketAddr::port` matches our ignored port list (`is_ignored_port`), and this socket
/// is in `MIRROR_SOCKETS`, then this function will move the socket to `BYPASS_SOCKETS`.
pub(super) fn bind(sockfd: c_int, address: SocketAddr) -> Result<(), LayerError> {
    debug!("bind -> sockfd {:#?} | address {:#?}", sockfd, address);

    // New fake address when it's a socket that we're interested in.
    let updated_address = if is_ignored_port(address.port()) {
        Ok(address)
    } else if address.is_ipv4() {
        Ok(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))
    } else if address.is_ipv6() {
        Ok(SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 0))
    } else {
        // TODO: Is this even possible?
        Err(LayerError::UnsupportedDomain(address))
    }?;

    {
        let mirror_sockets = MIRROR_SOCKETS.lock().unwrap();
        let bypass_sockets = BYPASS_SOCKETS.lock().unwrap();

        if let Some(socket) = mirror_sockets
            .get(&sockfd)
            .map(|mirror_socket| &mirror_socket.inner)
            .or_else(|| bypass_sockets.get(&sockfd))
        {
            Ok(socket.bind(&SockAddr::from(updated_address))?)
        } else {
            Err(LayerError::LocalFDNotFound(sockfd))
        }?;
    }

    // Put this socket into the bypassed list if it fails the `is_ignored_port` check, now that
    // we have more information about it (address).
    if is_ignored_port(updated_address.port()) {
        warn!(
            "bind -> sockfd {:#?} has an ignored port {:#?}, moving it to bypass list!",
            sockfd,
            updated_address.port()
        );

        let not_mirror_socket = MIRROR_SOCKETS.lock().unwrap().remove(&sockfd).unwrap();

        BYPASS_SOCKETS
            .lock()
            .unwrap()
            .insert(sockfd, not_mirror_socket.inner);
    }

    if let Some(mirror_socket) = MIRROR_SOCKETS.lock().unwrap().get_mut(&sockfd) {
        let bound = Bound {
            address,
            mirror_address: updated_address,
        };

        mirror_socket.state = SocketState::Bound(bound);
    }

    Ok(())
}

/// Bind the socket to a fake, local port, and subscribe to the agent on the real port.
/// Messages received from the agent on the real port will later be routed to the fake local port.
pub(super) fn listen(sockfd: RawFd, backlog: c_int) -> Result<(), LayerError> {
    debug!("listen -> sockfd {:#?} | backlog {:#?}", sockfd, backlog);

    {
        let mirror_sockets = MIRROR_SOCKETS.lock().unwrap();
        let bypass_sockets = BYPASS_SOCKETS.lock().unwrap();

        if let Some(socket) = mirror_sockets
            .get(&sockfd)
            .map(|mirror_socket| &mirror_socket.inner)
            .or_else(|| bypass_sockets.get(&sockfd))
        {
            Ok(socket.listen(backlog)?)
        } else {
            Err(LayerError::LocalFDNotFound(sockfd))
        }?;
    }

    if let Some(mirror_socket) = MIRROR_SOCKETS.lock().unwrap().get_mut(&sockfd) {
        let Bound {
            address,
            mirror_address,
        } = mirror_socket.get_bound()?;

        let sender = unsafe { HOOK_SENDER.as_ref().unwrap() };

        let listen_data = Listen {
            fake_port: mirror_address.port(),
            real_port: address.port(),
            fd: sockfd,
            ipv6: mirror_address.is_ipv6(),
        };

        sender.blocking_send(HookMessage::Tcp(HookMessageTcp::Listen(listen_data)))?;
    }

    Ok(())
}

pub(super) fn connect(sockfd: RawFd, address: SocketAddr) -> Result<(), LayerError> {
    {
        let mirror_sockets = MIRROR_SOCKETS.lock().unwrap();
        let bypass_sockets = BYPASS_SOCKETS.lock().unwrap();

        if let Some(socket) = mirror_sockets
            .get(&sockfd)
            .map(|mirror_socket| &mirror_socket.inner)
            .or_else(|| bypass_sockets.get(&sockfd))
        {
            Ok(socket.connect(&SockAddr::from(address))?)
        } else {
            Err(LayerError::LocalFDNotFound(sockfd))
        }
    }
}

/// Resolve fake local address to real remote address. (IP & port of incoming traffic on the
/// cluster)
/// See https://github.com/rust-lang/rust-clippy/issues/8963
pub(super) fn getpeername(sockfd: RawFd) -> Result<SockAddr, LayerError> {
    {
        let mirror_sockets = MIRROR_SOCKETS.lock().unwrap();
        let bypass_sockets = BYPASS_SOCKETS.lock().unwrap();

        if let Some(socket) = mirror_sockets
            .get(&sockfd)
            .map(|mirror_socket| &mirror_socket.inner)
            .or_else(|| bypass_sockets.get(&sockfd))
        {
            Ok(socket.peer_addr()?)
        } else {
            Err(LayerError::LocalFDNotFound(sockfd))
        }
    }
}

/// Resolve the fake local address to the real local address.
#[allow(clippy::significant_drop_in_scrutinee)]
/// See https://github.com/rust-lang/rust-clippy/issues/8963
pub(super) fn getsockname(sockfd: RawFd) -> Result<SockAddr, LayerError> {
    {
        let mirror_sockets = MIRROR_SOCKETS.lock().unwrap();
        let bypass_sockets = BYPASS_SOCKETS.lock().unwrap();

        if let Some(socket) = mirror_sockets
            .get(&sockfd)
            .map(|mirror_socket| &mirror_socket.inner)
            .or_else(|| bypass_sockets.get(&sockfd))
        {
            Ok(socket.local_addr()?)
        } else {
            Err(LayerError::LocalFDNotFound(sockfd))
        }
    }
}

/// When the fd is "ours", we accept and recv the first bytes that contain metadata on the
/// connection to be set in our lock This enables us to have a safe way to get "remote" information
/// (remote ip, port, etc).
pub(super) fn accept(sockfd: RawFd) -> Result<(RawFd, SockAddr), LayerError> {
    let (accepted_socket, accepted_address) = {
        let mirror_sockets = MIRROR_SOCKETS.lock().unwrap();
        let bypass_sockets = BYPASS_SOCKETS.lock().unwrap();

        if let Some(socket) = mirror_sockets
            .get(&sockfd)
            .map(|mirror_socket| &mirror_socket.inner)
            .or_else(|| bypass_sockets.get(&sockfd))
        {
            Ok(socket.accept()?)
        } else {
            Err(LayerError::LocalFDNotFound(sockfd))
        }
    }?;

    let accepted_fd = accepted_socket.as_raw_fd();

    if let Some(remote_address) = CONNECTION_QUEUE
        .lock()
        .unwrap()
        .get(&sockfd)
        .map(|socket_info| socket_info.address)
    {
        let connected = Connected {
            remote_address,
            local_address: getsockname(sockfd)?.as_socket().unwrap(),
        };

        let accepted_socket = MirrorSocket {
            inner: accepted_socket,
            state: SocketState::Connected(connected),
        };

        MIRROR_SOCKETS
            .lock()
            .unwrap()
            .insert(accepted_fd, accepted_socket);
    } else {
        BYPASS_SOCKETS
            .lock()
            .unwrap()
            .insert(accepted_fd, accepted_socket);
    }

    Ok((accepted_fd, accepted_address))
}

pub(super) fn dup(sockfd: RawFd) -> Result<RawFd, LayerError> {
    let mut mirror_sockets = MIRROR_SOCKETS.lock().unwrap();
    let mut bypass_sockets = BYPASS_SOCKETS.lock().unwrap();

    if let Some(mirror_socket) = mirror_sockets.get(&sockfd) {
        let duplicated_socket = mirror_socket.inner.try_clone()?;
        let duplicated_fd = duplicated_socket.as_raw_fd();

        let new_mirror_socket = MirrorSocket {
            inner: duplicated_socket,
            state: mirror_socket.state.clone(),
        };

        mirror_sockets.insert(duplicated_fd, new_mirror_socket);

        Ok(duplicated_fd)
    } else if let Some(bypass_socket) = bypass_sockets.get(&sockfd) {
        let duplicated_socket = bypass_socket.try_clone()?;
        let duplicated_fd = duplicated_socket.as_raw_fd();

        bypass_sockets.insert(duplicated_fd, duplicated_socket);

        Ok(duplicated_fd)
    } else {
        Err(LayerError::LocalFDNotFound(sockfd))
    }
}

pub(super) fn fcntl(orig_fd: c_int, cmd: c_int, fcntl_fd: i32) -> Result<c_int, LayerError> {
    todo!()
}
