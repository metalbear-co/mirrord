use std::os::unix::{io::RawFd, prelude::*};

use libc::c_int;
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use tracing::{debug, trace, warn};

use super::*;
use crate::{
    error::LayerError,
    message::HookMessage,
    tcp::{HookMessageTcp, Listen},
    HOOK_SENDER,
};

/// Create the socket, add it to `MIRROR_SOCKETS` if it's successful, plus protocol and domain
/// matches our expected type (Tcpv4/v6).
pub(super) fn socket(domain: c_int, type_: c_int, protocol: c_int) -> Result<RawFd, LayerError> {
    trace!(
        "socket -> domain {:#?} | type {:#?} | protocol {:#?}",
        domain,
        type_,
        protocol
    );

    let socket = Socket::new_raw(
        Domain::from(domain),
        Type::from(type_),
        Some(Protocol::from(protocol)),
    )?;

    // We don't handle non Tcpv4 sockets
    if !((domain == libc::AF_INET) || (domain == libc::AF_INET6) && (type_ & libc::SOCK_STREAM) > 0)
    {
        debug!(
            "socket -> Skipping non tcp socket {:#?} | domain {:#?} | type {:#?} | protocol {:#?}",
            socket, domain, type_, protocol
        );

        // Transfers ownership of socket to the caller.
        Ok(socket.into_raw_fd())
    } else {
        let fd = socket.as_raw_fd();
        let mirror_socket = MirrorSocket {
            inner: socket,
            state: SocketState::Initialized,
        };

        {
            trace!(
                "socket -> before lock we have fd {:#?} and mirror {:#?} | poison {:#?}",
                fd,
                mirror_socket,
                MIRROR_SOCKETS.is_poisoned()
            );

            let socket_result = MIRROR_SOCKETS.try_lock().and_then(|mut sockets| {
                let _ = sockets.insert(fd, mirror_socket);
                Ok(())
            });
            trace!("socket -> socket_result {:#?}", socket_result);
            socket_result.map_err(|fail| LayerError::SocketInvalidState)?;

            // MIRROR_SOCKETS.lock().unwrap().insert(fd, mirror_socket);
        }

        Ok(fd)
    }
}

/// TODO(alex) [low] 2022-06-27: Document why.
fn mirror_address(address: SocketAddr) -> Result<SocketAddr, LayerError> {
    // New fake address when it's a socket that we're interested in.
    if address.is_ipv4() {
        Ok(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))
    } else if address.is_ipv6() {
        Ok(SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 0))
    } else {
        // TODO: Is this even possible?
        Err(LayerError::UnsupportedDomain(address))
    }
}

/// Binds the socket, no matter to which list it belongs (`MIRROR_SOCKETS`, or `BYPASS_SOCKETS`),
/// but if the `SocketAddr::port` matches our ignored port list (`is_ignored_port`), and this socket
/// is in `MIRROR_SOCKETS`, then this function will move the socket to `BYPASS_SOCKETS`.
pub(super) fn bind(sockfd: c_int, address: SocketAddr) -> Result<(), LayerError> {
    trace!("bind -> sockfd {:#?} | address {:#?}", sockfd, address);

    if is_ignored_port(address.port()) {
        warn!(
            "bind -> sockfd {:#?} has an ignored port {:#?}!",
            sockfd,
            address.port()
        );

        let bypass_fd = MIRROR_SOCKETS
            .try_lock()?
            .remove(&sockfd)
            .ok_or(LayerError::LocalFdNotFound(sockfd))
            .and_then(|drop_socket| Ok(drop_socket.inner.into_raw_fd()))?;

        Err(LayerError::BypassBind(bypass_fd))
    } else {
        let mirror_address = mirror_address(address)?;

        MIRROR_SOCKETS
            .try_lock()?
            .get_mut(&sockfd)
            .ok_or(LayerError::LocalFdNotFound(sockfd))
            .and_then(|mirror_socket| {
                mirror_socket.inner.bind(&SockAddr::from(mirror_address))?;

                let bound = Bound {
                    address,
                    mirror_address,
                };

                mirror_socket.state = SocketState::Bound(bound);

                Ok(())
            })
    }
}

/// Bind the socket to a fake, local port, and subscribe to the agent on the real port.
/// Messages received from the agent on the real port will later be routed to the fake local port.
pub(super) fn listen(sockfd: RawFd, backlog: c_int) -> Result<(), LayerError> {
    trace!("listen -> sockfd {:#?} | backlog {:#?}", sockfd, backlog);

    let Bound {
        address,
        mirror_address,
    } = MIRROR_SOCKETS
        .try_lock()?
        .get(&sockfd)
        .ok_or(LayerError::LocalFdNotFound(sockfd))
        .and_then(|mirror_socket| mirror_socket.get_bound())?;

    let sender = unsafe { HOOK_SENDER.as_ref().unwrap() };

    let listen_data = Listen {
        fake_port: mirror_address.port(),
        real_port: address.port(),
        fd: sockfd,
        ipv6: mirror_address.is_ipv6(),
    };

    sender.blocking_send(HookMessage::Tcp(HookMessageTcp::Listen(listen_data)))?;

    MIRROR_SOCKETS
        .try_lock()?
        .get_mut(&sockfd)
        .ok_or(LayerError::LocalFdNotFound(sockfd))
        .and_then(|mirror_socket| {
            mirror_socket.inner.listen(backlog)?;

            mirror_socket.state = SocketState::Listening(mirror_socket.get_bound()?);

            Ok(())
        })
}

pub(super) fn connect(sockfd: RawFd, address: SocketAddr) -> Result<(), LayerError> {
    trace!("connect -> sockfd {:#?} | address {:#?}", sockfd, address);

    MIRROR_SOCKETS
        .try_lock()?
        .get(&sockfd)
        .ok_or(LayerError::LocalFdNotFound(sockfd))
        .and_then(|mirror_socket| {
            mirror_socket.inner.connect(&SockAddr::from(address))?;

            let Bound {
                address,
                mirror_address,
            } = mirror_socket.get_bound()?;

            mirror_socket.state = SocketState::Connected(Connected {
                remote_address: todo!(),
                local_address: todo!(),
            });

            Ok(())
        })
}

/// Resolve fake local address to real remote address. (IP & port of incoming traffic on the
/// cluster)
pub(super) fn getpeername(sockfd: RawFd) -> Result<SockAddr, LayerError> {
    trace!("getpeername -> sockfd {:#?}", sockfd);

    MIRROR_SOCKETS
        .try_lock()?
        .get(&sockfd)
        .ok_or(LayerError::LocalFdNotFound(sockfd))
        .and_then(|mirror_socket| Ok(mirror_socket.inner.peer_addr()?))
}

/// Resolve the fake local address to the real local address.
pub(super) fn getsockname(sockfd: RawFd) -> Result<SockAddr, LayerError> {
    trace!("getsockname -> sockfd {:#?}", sockfd);

    MIRROR_SOCKETS
        .try_lock()?
        .get(&sockfd)
        .ok_or(LayerError::LocalFdNotFound(sockfd))
        .and_then(|mirror_socket| Ok(mirror_socket.inner.local_addr()?))
}

/// When the fd is "ours", we accept and recv the first bytes that contain metadata on the
/// connection to be set in our lock This enables us to have a safe way to get "remote" information
/// (remote ip, port, etc).
pub(super) fn accept(sockfd: RawFd, flags: Option<i32>) -> Result<(RawFd, SockAddr), LayerError> {
    trace!("accept -> sockfd {:#?}", sockfd);

    // TODO(alex) [high] 2022-06-28: `accept` is a blocking call, so we hold this lock for way too
    // long.
    let (accepted_socket, accepted_address) = {
        MIRROR_SOCKETS
            .try_lock()?
            .get(&sockfd)
            .ok_or(LayerError::LocalFdNotFound(sockfd))
            .and_then(|mirror_socket| {
                trace!("accept -> mirror {:#?}", mirror_socket);

                // TODO(alex) [high] 2022-06-27: We block here!
                let accepted_result = match flags {
                    Some(flags) => mirror_socket.inner.accept4(flags),
                    None => mirror_socket.inner.accept(),
                }?;

                trace!("accept -> unblocked!");

                Ok(accepted_result)
            })
    }?;

    let accepted_fd = accepted_socket.as_raw_fd();

    trace!("accept -> mutex is {:#?}", MIRROR_SOCKETS);
    if let Some(remote_address) = CONNECTION_QUEUE
        .try_lock()?
        .get(&sockfd)
        .map(|socket_info| socket_info.address)
    {
        trace!("accept -> remote_address {:#?}", remote_address);
        let connected = Connected {
            remote_address,
            local_address: getsockname(sockfd)?.as_socket().unwrap(),
        };

        let accepted_socket = MirrorSocket {
            inner: accepted_socket,
            state: SocketState::Connected(connected),
        };

        trace!("accept -> accepted_socket {:#?}", accepted_socket);

        MIRROR_SOCKETS
            .try_lock()?
            .insert(accepted_fd, accepted_socket);
    }

    trace!(
        "accept -> Success fd {:#?} | address {:#?}",
        accepted_fd,
        accepted_address
    );

    Ok((accepted_fd, accepted_address))
}

pub(super) fn dup(sockfd: RawFd) -> Result<RawFd, LayerError> {
    trace!("dup -> sockfd {:#?}", sockfd);

    let mut mirror_sockets = MIRROR_SOCKETS.try_lock()?;

    if let Some(mirror_socket) = mirror_sockets.get(&sockfd) {
        let duplicated_socket = mirror_socket.inner.try_clone()?;
        let duplicated_fd = duplicated_socket.as_raw_fd();

        let new_mirror_socket = MirrorSocket {
            inner: duplicated_socket,
            state: mirror_socket.state.clone(),
        };

        mirror_sockets.insert(duplicated_fd, new_mirror_socket);

        Ok(duplicated_fd)
    } else {
        Err(LayerError::LocalFdNotFound(sockfd))
    }
}

pub(super) fn fcntl(orig_fd: c_int, cmd: c_int, fcntl_fd: i32) -> Result<c_int, LayerError> {
    todo!()
}
