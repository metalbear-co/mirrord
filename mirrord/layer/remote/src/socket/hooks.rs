use std::{
    cmp, io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6},
    ops::Not,
    os::unix::io::RawFd,
    ptr,
    sync::Arc,
};

use libc::{c_int, sockaddr, socklen_t};
use mirrord_config::feature::network::incoming::{IncomingConfig, IncomingMode};
use mirrord_intproxy_protocol::{PortSubscribe, RemoteAcceptVerdict};
use mirrord_layer_core::HookManager;
use mirrord_layer_lib::{
    detour::{Bypass, Detour},
    error::HookError,
    proxy_connection::make_proxy_request_with_response,
    setup::setup,
    socket::{
        Bound, Connected, SOCKETS, SocketAddrExt, SocketKind, SocketState, UserSocket, ops::socket,
    },
};
use mirrord_layer_macro::hook_guard_fn;
use nix::{errno::Errno, sys::socket::SockaddrStorage};
use socket2::SockAddr;
use tracing::{debug, error, warn};

use super::remote_accept::{accept_record, handoff_remote_accepted};

#[inline]
fn fill_address(
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    new_address: SockAddr,
) -> Detour<i32> {
    let result = if address.is_null() {
        Ok(0)
    } else if address_len.is_null() {
        Err(HookError::NullPointer)
    } else {
        unsafe {
            let len = cmp::min(*address_len as usize, new_address.len() as usize);

            ptr::copy_nonoverlapping(new_address.as_ptr() as *const u8, address as *mut u8, len);
            *address_len = new_address.len();
        }

        Ok(0)
    }?;

    Detour::Success(result)
}

#[inline]
fn is_ignored_port(addr: &SocketAddr) -> bool {
    addr.port() == 0
}

fn is_ignored_tcp_port(addr: &SocketAddr, config: &IncomingConfig) -> bool {
    let mapped_port = setup()
        .incoming_config()
        .port_mapping
        .get_by_left(&addr.port())
        .copied()
        .unwrap_or_else(|| addr.port());

    let have_whitelist_and_port_is_not_whitelisted = config
        .ports
        .as_ref()
        .is_some_and(|ports| ports.contains(&mapped_port).not());

    is_ignored_port(addr) || have_whitelist_and_port_is_not_whitelisted
}

fn bind_similar_address(
    sockfd: c_int,
    requested: SocketAddr,
    can_use_random_port: bool,
) -> Detour<()> {
    let address = SockaddrStorage::from(requested);
    if nix::sys::socket::bind(sockfd, &address).is_ok() {
        return Detour::Success(());
    }

    if can_use_random_port {
        let address = SockaddrStorage::from(SocketAddr::new(requested.ip(), 0));
        if nix::sys::socket::bind(sockfd, &address).is_ok() {
            return Detour::Success(());
        }
    }

    if requested.ip().is_loopback() || requested.ip().is_unspecified() {
        return Detour::Error(io::Error::last_os_error().into());
    }

    let new_ip = if requested.ip().is_ipv4() {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        IpAddr::V6(Ipv6Addr::UNSPECIFIED)
    };
    let address = SockaddrStorage::from(SocketAddr::new(new_ip, requested.port()));
    if nix::sys::socket::bind(sockfd, &address).is_ok() {
        return Detour::Success(());
    }

    if can_use_random_port {
        let address = SockaddrStorage::from(SocketAddr::new(new_ip, 0));
        if nix::sys::socket::bind(sockfd, &address).is_ok() {
            return Detour::Success(());
        }
    }

    Detour::Error(io::Error::last_os_error().into())
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn socket_detour(
    domain: c_int,
    type_: c_int,
    protocol: c_int,
) -> c_int {
    unsafe {
        let call_original = || {
            let socket_result = FN_SOCKET(domain, type_, protocol);
            if socket_result == -1 {
                Detour::Error(std::io::Error::last_os_error().into())
            } else {
                Detour::Success(socket_result)
            }
        };
        socket(call_original, domain, type_, protocol)
            .unwrap_or_bypass_with(|_| FN_SOCKET(domain, type_, protocol))
    }
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn close_detour(fd: c_int) -> c_int {
    unsafe {
        let res = FN_CLOSE(fd);
        close_layer_fd(fd);
        res
    }
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn bind_detour(
    sockfd: c_int,
    raw_address: *const sockaddr,
    address_length: socklen_t,
) -> c_int {
    unsafe {
        bind(sockfd, raw_address, address_length)
            .unwrap_or_bypass_with(|_| FN_BIND(sockfd, raw_address, address_length))
    }
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn listen_detour(sockfd: RawFd, backlog: c_int) -> c_int {
    unsafe { listen(sockfd, backlog).unwrap_or_bypass_with(|_| FN_LISTEN(sockfd, backlog)) }
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn getsockname_detour(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> c_int {
    unsafe {
        getsockname(sockfd, address, address_len)
            .unwrap_or_bypass_with(|_| FN_GETSOCKNAME(sockfd, address, address_len))
    }
}

#[hook_guard_fn]
pub(crate) unsafe extern "C" fn accept_detour(
    sockfd: c_int,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> c_int {
    unsafe {
        let accept_result = FN_ACCEPT(sockfd, address, address_len);

        if accept_result == -1 {
            accept_result
        } else {
            accept(sockfd, address, address_len, accept_result).unwrap_or_bypass(accept_result)
        }
    }
}

#[cfg(target_os = "linux")]
#[hook_guard_fn]
pub(crate) unsafe extern "C" fn accept4_detour(
    sockfd: c_int,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    flags: c_int,
) -> c_int {
    unsafe {
        let accept_result = FN_ACCEPT4(sockfd, address, address_len, flags);

        if accept_result == -1 {
            accept_result
        } else {
            accept(sockfd, address, address_len, accept_result).unwrap_or_bypass(accept_result)
        }
    }
}

#[cfg(target_os = "linux")]
#[hook_guard_fn]
#[allow(non_snake_case)]
pub(super) unsafe extern "C" fn uv__accept4_detour(
    sockfd: c_int,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    flags: c_int,
) -> c_int {
    unsafe {
        tracing::trace!("uv__accept4_detour -> sockfd {:#?}", sockfd);

        accept4_detour(sockfd, address, address_len, flags)
    }
}

#[cfg(target_os = "macos")]
#[hook_guard_fn]
pub(super) unsafe extern "C" fn accept_nocancel_detour(
    sockfd: c_int,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> c_int {
    unsafe {
        let accept_result = FN_ACCEPT_NOCANCEL(sockfd, address, address_len);

        if accept_result == -1 {
            accept_result
        } else {
            accept(sockfd, address, address_len, accept_result).unwrap_or_bypass(accept_result)
        }
    }
}

fn close_layer_fd(fd: c_int) {
    if let Some(socket) = SOCKETS.lock().expect("SOCKETS lock failed").remove(&fd) {
        let socket_cloned = socket.as_ref().clone();

        let weak = Arc::downgrade(&socket);
        std::mem::drop(socket);

        if weak.strong_count() == 0 {
            socket_cloned.close();
        }
    }
}

#[mirrord_layer_macro::instrument(level = tracing::Level::TRACE, fields(pid = std::process::id()), ret, skip(raw_address))]
fn bind(sockfd: c_int, raw_address: *const sockaddr, address_length: socklen_t) -> Detour<i32> {
    let requested_address = SocketAddr::try_from_raw(raw_address, address_length)?;
    let requested_port = requested_address.port();
    let incoming_config = setup().incoming_config();
    let mut socket = {
        SOCKETS
            .lock()?
            .remove(&sockfd)
            .ok_or(Bypass::LocalFdNotFound(sockfd))
            .and_then(|socket| {
                if !matches!(socket.state, SocketState::Initialized) {
                    Err(Bypass::InvalidState(sockfd))
                } else {
                    Ok(socket)
                }
            })?
    };

    let domain_valid = match socket.domain {
        libc::AF_INET => requested_address.is_ipv4(),
        libc::AF_INET6 => requested_address.is_ipv6(),
        _ => false,
    };
    if !domain_valid {
        Err(HookError::InvalidBindAddressForDomain)?;
    }

    if requested_address.port() != 0
        && SOCKETS
            .lock()?
            .iter()
            .any(|(_, socket)| match &socket.state {
                SocketState::Initialized | SocketState::Connected(_) => false,
                SocketState::Bound { bound, .. } | SocketState::Listening(bound) => {
                    bound.requested_address == requested_address
                }
            })
    {
        Err(HookError::AddressAlreadyBound(requested_address))?;
    }

    let listen_port = incoming_config
        .listen_ports
        .get_by_left(&requested_address.port())
        .copied();

    let will_not_trigger_subscription = (incoming_config.ignore_localhost
        && requested_address.ip().is_loopback())
        || ((matches!(socket.kind, SocketKind::Tcp(_)))
            && is_ignored_tcp_port(&requested_address, incoming_config)
            || setup().is_debugger_port(&requested_address)
            || incoming_config.ignore_ports.contains(&requested_port));

    if will_not_trigger_subscription && listen_port.is_none() {
        return Detour::Bypass(Bypass::IgnoredInIncoming(requested_address));
    }

    if let Some(port) = listen_port {
        let mut address = requested_address;
        address.set_port(port);
        bind_similar_address(sockfd, address, false)
    } else {
        bind_similar_address(sockfd, requested_address, true)
    }?;

    let Ok(address) = nix::sys::socket::getsockname::<SockaddrStorage>(sockfd) else {
        let error = io::Error::last_os_error();
        tracing::error!(
            sockfd,
            %error,
            "Failed to retrieve socket local address after intercepted bind."
        );
        return Detour::Error(error.into());
    };
    let address: SocketAddr = if let Some(ipv4) = address.as_sockaddr_in() {
        SocketAddrV4::from(*ipv4).into()
    } else if let Some(ipv6) = address.as_sockaddr_in6() {
        SocketAddrV6::from(*ipv6).into()
    } else {
        tracing::error!(
            %address,
            sockfd,
            "Failed to retrieve socket local address after intercepted bind. \
            Should be an IPv4 or IPv6 address.",
        );
        return Detour::Bypass(Bypass::AddressConversion);
    };

    Arc::get_mut(&mut socket).unwrap().state = SocketState::Bound {
        bound: Bound {
            requested_address,
            address,
        },
        is_only_bound: will_not_trigger_subscription,
    };

    SOCKETS.lock()?.insert(sockfd, socket);

    Errno::set_raw(0);
    Detour::Success(0)
}

#[mirrord_layer_macro::instrument(level = tracing::Level::TRACE, fields(pid = std::process::id()), ret)]
fn listen(sockfd: RawFd, backlog: c_int) -> Detour<i32> {
    let Some(mut socket) = SOCKETS.lock()?.remove(&sockfd) else {
        return Detour::Bypass(Bypass::LocalFdNotFound(sockfd));
    };

    let setup = setup();

    if matches!(setup.incoming_config().mode, IncomingMode::Off) {
        return Detour::Bypass(Bypass::DisabledIncoming);
    }

    if setup.targetless() {
        warn!(
            "Listening while running targetless. A targetless agent is not exposed by \
        any service. Therefore, letting this port bind happen locally instead of on the \
        cluster.",
        );
        return Detour::Bypass(Bypass::BindWhenTargetless);
    }

    match socket.state {
        SocketState::Bound {
            bound: Bound {
                requested_address,
                address,
            },
            is_only_bound,
        } if !is_only_bound => {
            let listen_result = unsafe { FN_LISTEN(sockfd, backlog) };
            if listen_result != 0 {
                let error = io::Error::last_os_error();
                error!("listen -> Failed `listen` sockfd {:#?}", sockfd);
                Err(error)?;
            }

            let mapped_port = setup
                .incoming_config()
                .port_mapping
                .get_by_left(&requested_address.port())
                .copied()
                .unwrap_or_else(|| requested_address.port());

            make_proxy_request_with_response(PortSubscribe {
                listening_on: address.into(),
                subscription: setup.incoming_mode().subscription(mapped_port),
            })??;

            debug!("daemon subscribed port {}", requested_address.port());

            Arc::get_mut(&mut socket).unwrap().state = SocketState::Listening(Bound {
                requested_address,
                address,
            });

            SOCKETS.lock()?.insert(sockfd, socket);

            Detour::Success(listen_result)
        }
        SocketState::Listening(_) => {
            debug!("second listen called");
            let listen_result = unsafe { FN_LISTEN(sockfd, backlog) };

            SOCKETS.lock()?.insert(sockfd, socket);

            if listen_result != 0 {
                let error = io::Error::last_os_error();
                error!("listen -> Failed `listen` sockfd {:#?}", sockfd);
                Err(error)?;
            }

            Detour::Success(listen_result)
        }
        SocketState::Bound { .. } | SocketState::Initialized | SocketState::Connected(_) => {
            Detour::Bypass(Bypass::InvalidState(sockfd))
        }
    }
}

fn getsockname(sockfd: RawFd, address: *mut sockaddr, address_len: *mut socklen_t) -> Detour<i32> {
    let socket = SOCKETS
        .lock()?
        .get(&sockfd)
        .ok_or(Bypass::LocalFdNotFound(sockfd))?
        .clone();

    let local_address: SockAddr = match &socket.state {
        SocketState::Bound {
            bound: Bound {
                requested_address,
                address,
            },
            ..
        }
        | SocketState::Listening(Bound {
            requested_address,
            address,
        }) => {
            if requested_address.port() == 0 {
                SocketAddr::new(requested_address.ip(), address.port()).into()
            } else {
                requested_address.to_owned().into()
            }
        }
        SocketState::Initialized | SocketState::Connected(_) => {
            return Detour::Bypass(Bypass::InvalidState(sockfd));
        }
    };

    fill_address(address, address_len, local_address)
}

fn duplicate_fd(fd: c_int) -> io::Result<c_int> {
    let duplicated_fd = unsafe { libc::dup(fd) };
    if duplicated_fd == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(duplicated_fd)
    }
}

fn close_raw_fd(fd: c_int) {
    unsafe {
        libc::close(fd);
    }
}

fn finalize_accepted_socket(
    fd: RawFd,
    domain: c_int,
    type_: c_int,
    protocol: c_int,
    local_address: SocketAddr,
    peer_address: SocketAddr,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> Detour<RawFd> {
    let state = SocketState::Connected(Connected {
        connection_id: None,
        remote_address: peer_address.into(),
        local_address: Some(local_address.into()),
        layer_address: None,
    });

    let new_socket = UserSocket::new(domain, type_, protocol, state, type_.try_into()?);
    fill_address(address, address_len, peer_address.into())?;
    SOCKETS.lock()?.insert(fd, Arc::new(new_socket));

    Detour::Success(fd)
}

fn passthrough_metadata(
    response: &mirrord_intproxy_protocol::RemoteAcceptResponse,
) -> Option<(SocketAddr, SocketAddr)> {
    accept_record(response.accept_id).map(|record| (record.local_address, record.peer_address))
}

#[mirrord_layer_macro::instrument(level = "trace", ret, skip(address, address_len))]
fn accept(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    new_fd: RawFd,
) -> Detour<RawFd> {
    let socket = SOCKETS
        .lock()?
        .get(&sockfd)
        .ok_or(Bypass::LocalFdNotFound(sockfd))?
        .clone();
    let socket = socket.as_ref().clone();

    let domain = socket.domain;
    let protocol = socket.protocol;
    let type_ = socket.type_;
    let listener_address = match socket.state {
        SocketState::Listening(Bound { address, .. }) => address,
        SocketState::Bound { .. } | SocketState::Initialized | SocketState::Connected(_) => {
            return Detour::Bypass(Bypass::InvalidState(sockfd));
        }
    };

    let Ok(local_address) = nix::sys::socket::getsockname::<SockaddrStorage>(new_fd) else {
        let error = io::Error::last_os_error();
        tracing::error!(
            new_fd,
            %error,
            "Failed to retrieve accepted socket local address after intercepted accept."
        );
        return Detour::Error(error.into());
    };
    let local_address: SocketAddr = if let Some(ipv4) = local_address.as_sockaddr_in() {
        SocketAddrV4::from(*ipv4).into()
    } else if let Some(ipv6) = local_address.as_sockaddr_in6() {
        SocketAddrV6::from(*ipv6).into()
    } else {
        tracing::error!(
            %local_address,
            new_fd,
            "Failed to retrieve accepted socket local address after intercepted accept. \
            Should be an IPv4 or IPv6 address."
        );
        return Detour::Bypass(Bypass::AddressConversion);
    };

    let Ok(peer_address) = nix::sys::socket::getpeername::<SockaddrStorage>(new_fd) else {
        let error = io::Error::last_os_error();
        tracing::error!(
            new_fd,
            %error,
            "Failed to retrieve accepted socket peer address after intercepted accept."
        );
        return Detour::Error(error.into());
    };
    let peer_address: SocketAddr = if let Some(ipv4) = peer_address.as_sockaddr_in() {
        SocketAddrV4::from(*ipv4).into()
    } else if let Some(ipv6) = peer_address.as_sockaddr_in6() {
        SocketAddrV6::from(*ipv6).into()
    } else {
        tracing::error!(
            %peer_address,
            new_fd,
            "Failed to retrieve accepted socket peer address after intercepted accept. \
            Should be an IPv4 or IPv6 address."
        );
        return Detour::Bypass(Bypass::AddressConversion);
    };

    let fallback_fd = duplicate_fd(new_fd).ok();
    let remote_accept_response =
        match handoff_remote_accepted(listener_address, local_address, peer_address, new_fd) {
            Ok(response) => response,
            Err(error) => {
                warn!(
                    %error,
                    listener_address = %listener_address,
                    peer_address = %peer_address,
                    "remote accepted fd handoff failed; returning accepted fd to the application"
                );

                if let Some(fallback_fd) = fallback_fd {
                    close_raw_fd(new_fd);
                    return finalize_accepted_socket(
                        fallback_fd,
                        domain,
                        type_,
                        protocol,
                        local_address,
                        peer_address,
                        address,
                        address_len,
                    );
                }

                return finalize_accepted_socket(
                    new_fd,
                    domain,
                    type_,
                    protocol,
                    local_address,
                    peer_address,
                    address,
                    address_len,
                );
            }
        };

    match remote_accept_response.verdict {
        RemoteAcceptVerdict::Claim => {
            if let Some(fallback_fd) = fallback_fd {
                close_raw_fd(fallback_fd);
            }
            close_raw_fd(new_fd);
            Detour::Error(HookError::IO(io::Error::from_raw_os_error(
                libc::ECONNABORTED,
            )))
        }
        RemoteAcceptVerdict::Decline | RemoteAcceptVerdict::Passthrough => {
            let accepted_fd = if let Some(fallback_fd) = fallback_fd {
                close_raw_fd(new_fd);
                fallback_fd
            } else {
                new_fd
            };

            let (local_address, peer_address) = if matches!(
                remote_accept_response.verdict,
                RemoteAcceptVerdict::Passthrough
            ) {
                if let Some((local_address, peer_address)) =
                    passthrough_metadata(&remote_accept_response)
                {
                    tracing::trace!(
                        accept_id = remote_accept_response.accept_id,
                        listener_address = %remote_accept_response.listener_address,
                        local_address = %local_address,
                        peer_address = %peer_address,
                        "remote accepted passthrough resolved to canonical origin record"
                    );
                    (local_address, peer_address)
                } else {
                    (
                        remote_accept_response.local_address,
                        remote_accept_response.peer_address,
                    )
                }
            } else {
                (
                    remote_accept_response.local_address,
                    remote_accept_response.peer_address,
                )
            };

            finalize_accepted_socket(
                accepted_fd,
                domain,
                type_,
                protocol,
                local_address,
                peer_address,
                address,
                address_len,
            )
        }
    }
}

pub(crate) unsafe fn enable_socket_hooks(hook_manager: &mut HookManager) {
    unsafe {
        mirrord_layer_core::replace!(hook_manager, "socket", socket_detour, FnSocket, FN_SOCKET);
        mirrord_layer_core::replace!(hook_manager, "close", close_detour, FnClose, FN_CLOSE);
        mirrord_layer_core::replace!(hook_manager, "bind", bind_detour, FnBind, FN_BIND);
        mirrord_layer_core::replace!(hook_manager, "listen", listen_detour, FnListen, FN_LISTEN);
        mirrord_layer_core::replace!(
            hook_manager,
            "getsockname",
            getsockname_detour,
            FnGetsockname,
            FN_GETSOCKNAME
        );
        mirrord_layer_core::replace!(hook_manager, "accept", accept_detour, FnAccept, FN_ACCEPT);
        #[cfg(target_os = "linux")]
        {
            mirrord_layer_core::replace!(
                hook_manager,
                "accept4",
                accept4_detour,
                FnAccept4,
                FN_ACCEPT4
            );
            mirrord_layer_core::replace!(
                hook_manager,
                "uv__accept4",
                uv__accept4_detour,
                FnAccept4,
                FN_ACCEPT4
            );
        }
        #[cfg(target_os = "macos")]
        {
            mirrord_layer_core::replace!(
                hook_manager,
                "accept$NOCANCEL",
                accept_nocancel_detour,
                FnAccept_nocancel,
                FN_ACCEPT_NOCANCEL
            );
        }
    }
}
