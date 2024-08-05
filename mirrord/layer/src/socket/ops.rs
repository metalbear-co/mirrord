use alloc::ffi::CString;
use core::{ffi::CStr, mem};
use std::{
    collections::HashMap,
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream},
    os::{
        fd::{BorrowedFd, FromRawFd, IntoRawFd},
        unix::io::RawFd,
    },
    path::PathBuf,
    ptr,
    sync::{Arc, Mutex, OnceLock},
};

use errno::set_errno;
use libc::{c_int, c_void, hostent, sockaddr, socklen_t, AF_UNIX};
use mirrord_config::feature::network::incoming::{IncomingConfig, IncomingMode};
use mirrord_intproxy_protocol::{
    ConnMetadataRequest, ConnMetadataResponse, NetProtocol, OutgoingConnectRequest,
    OutgoingConnectResponse, PortSubscribe,
};
use mirrord_protocol::{
    dns::{GetAddrInfoRequest, LookupRecord},
    file::{OpenFileResponse, OpenOptionsInternal, ReadFileResponse},
};
use nix::sys::socket::{sockopt, SockaddrLike, SockaddrStorage};
use socket2::SockAddr;
use tracing::{error, trace, Level};

use super::{hooks::*, *};
use crate::{
    detour::{Detour, OnceLockExt, OptionDetourExt, OptionExt},
    error::HookError,
    file::{self, OPEN_FILES},
};

/// Holds the pair of [`IpAddr`] with their hostnames, resolved remotely through
/// [`remote_getaddrinfo`].
///
/// Used by [`connect_outgoing`] to retrieve the hostname from the address that the user called
/// [`connect`] with, so we can resolve it locally when neccessary.
pub(super) static REMOTE_DNS_REVERSE_MAPPING: LazyLock<Mutex<HashMap<IpAddr, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Hostname initialized from the agent with [`gethostname`].
pub(crate) static HOSTNAME: OnceLock<CString> = OnceLock::new();

/// Globals used by `gethostbyname`.
static mut GETHOSTBYNAME_HOSTNAME: Option<CString> = None;
static mut GETHOSTBYNAME_ALIASES_STR: Option<Vec<CString>> = None;

/// **Safety**:
/// There is a potential UB trigger here, as [`libc::hostent`] uses this as an `*mut _`, while we
/// have `*const _`. As this is being filled to fulfill the contract of a deprecated function, I
/// (alex) don't think we're going to hit this issue ever.
static mut GETHOSTBYNAME_ALIASES_PTR: Option<Vec<*const i8>> = None;
static mut GETHOSTBYNAME_ADDRESSES_VAL: Option<Vec<[u8; 4]>> = None;
static mut GETHOSTBYNAME_ADDRESSES_PTR: Option<Vec<*mut u8>> = None;

/// Global static that the user will receive when calling [`gethostbyname`].
///
/// **Safety**:
/// Even though we fill it with some `*const _` while it expects `*mut _`, it shouldn't be a problem
/// as the user will most likely be doing a deep copy if they want to mess around with this struct.
static mut GETHOSTBYNAME_HOSTENT: hostent = hostent {
    h_name: ptr::null_mut(),
    h_aliases: ptr::null_mut(),
    h_addrtype: 0,
    h_length: 0,
    h_addr_list: ptr::null_mut(),
};

/// Helper struct for connect results where we want to hold the original errno
/// when result is -1 (error) because sometimes it's not a real error (EINPROGRESS/EINTR)
/// and the caller should have the original value.
/// In order to use this struct correctly, after calling connect convert the return value using
/// this `.into/.from` then the detour would call `.into` on the struct to convert to i32
/// and would set the according errno.
#[derive(Debug)]
pub(super) struct ConnectResult {
    result: i32,
    error: Option<errno::Errno>,
}

impl ConnectResult {
    pub(super) fn is_failure(&self) -> bool {
        matches!(self.error, Some(err) if err.0 != libc::EINTR && err.0 != libc::EINPROGRESS)
    }
}

impl From<i32> for ConnectResult {
    fn from(result: i32) -> Self {
        if result == -1 {
            ConnectResult {
                result,
                error: Some(errno::errno()),
            }
        } else {
            ConnectResult {
                result,
                error: None,
            }
        }
    }
}

impl From<ConnectResult> for i32 {
    fn from(connect_result: ConnectResult) -> Self {
        if let Some(error) = connect_result.error {
            errno::set_errno(error);
        }
        connect_result.result
    }
}

/// Create the socket, add it to SOCKETS if successful and matching protocol and domain (Tcpv4/v6)
#[mirrord_layer_macro::instrument(level = Level::TRACE, fields(pid = std::process::id()), ret)]
pub(super) fn socket(domain: c_int, type_: c_int, protocol: c_int) -> Detour<RawFd> {
    let socket_kind = type_.try_into()?;

    if !((domain == libc::AF_INET) || (domain == libc::AF_INET6) || (domain == libc::AF_UNIX)) {
        Err(Bypass::Domain(domain))
    } else {
        Ok(())
    }?;

    if domain == libc::AF_INET6 {
        return Detour::Error(HookError::SocketUnsuportedIpv6);
    }

    let socket_result = unsafe { FN_SOCKET(domain, type_, protocol) };

    let socket_fd = if socket_result == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(socket_result)
    }?;

    let new_socket = UserSocket::new(domain, type_, protocol, Default::default(), socket_kind);

    SOCKETS.lock()?.insert(socket_fd, Arc::new(new_socket));

    Detour::Success(socket_fd)
}

/// Tries to bind the given socket to a loopback or an unspecified address similar to the given
/// `requested_address`.
///
/// If the given `requested_address` is not a loopback and is specified, binds to either
/// [`Ipv4Addr::LOCALHOST`] or [`Ipv6Addr::UNSPECIFIED`].
#[mirrord_layer_macro::instrument(level = "trace", ret)]
fn bind_similar_address(sockfd: c_int, requested_address: &SocketAddr) -> Detour<()> {
    let addr = requested_address.ip();
    let port = requested_address.port();

    let address = if addr.is_loopback() || addr.is_unspecified() {
        *requested_address
    } else if addr.is_ipv4() {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port)
    } else {
        SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port)
    };

    let address = SockAddr::from(address);

    let bind_result = unsafe { FN_BIND(sockfd, address.as_ptr(), address.len()) };
    if bind_result != 0 {
        Detour::Error(io::Error::last_os_error().into())
    } else {
        Detour::Success(())
    }
}

/// Checks if given TCP port needs to be ignored
/// based on http_filter/ports logic
fn is_ignored_tcp_port(addr: &SocketAddr, config: &IncomingConfig) -> bool {
    let mapped_port = crate::setup()
        .incoming_config()
        .port_mapping
        .get_by_left(&addr.port())
        .copied()
        .unwrap_or_else(|| addr.port());
    let http_filter_used = config.mode == IncomingMode::Steal && config.http_filter.is_filter_set();

    // this is a bit weird but it makes more sense configured ports are the remote port
    // and not the local, so the check is done on the mapped port
    // see https://github.com/metalbear-co/mirrord/issues/2397
    let not_a_filtered_port = !config.http_filter.ports.contains(&mapped_port);

    let not_stolen_with_filter = !http_filter_used || not_a_filtered_port;

    // Unfiltered ports were specified and the requested port is not one of them, or an HTTP filter
    // is set and no unfiltered ports were specified.
    let not_whitelisted = config
        .ports
        .as_ref()
        .map(|ports| !ports.contains(&mapped_port))
        .unwrap_or(http_filter_used);

    if http_filter_used && not_a_filtered_port && config.ports.is_none() {
        // User specified a filter that does not include this port, and did not specify any
        // unfiltered ports.
        // It's plausible that the user did not know the port has to be in either port list to be
        // stolen when an HTTP filter is set, so show a warning.
        let port_text = if mapped_port != addr.port() {
            format!(
                "Remote port {mapped_port} (mapped from local port {})",
                addr.port()
            )
        } else {
            format!("Port {mapped_port}")
        };
        warn!(
            "{port_text} was not included in the filtered ports, and also not in the non-filtered \
            ports, and will therefore be bound locally. If this is intentional, ignore this \
            warning. If you want the http filter to apply to this port, add it to \
            `feature.network.incoming.http_filter.ports`. \
            If you want to steal all the traffic of this port, add it to \
            `feature.network.incoming.ports`."
        );
    }

    is_ignored_port(addr) || (not_stolen_with_filter && not_whitelisted)
}

/// If the socket is not found in [`SOCKETS`], bypass.
/// Otherwise, if it's not an ignored port, bind (possibly with a fallback to random port) and
/// update socket state in [`SOCKETS`]. If it's an ignored port, remove the socket from [`SOCKETS`].
#[mirrord_layer_macro::instrument(level = Level::TRACE, fields(pid = std::process::id()), ret, skip(raw_address))]
pub(super) fn bind(
    sockfd: c_int,
    raw_address: *const sockaddr,
    address_length: socklen_t,
) -> Detour<i32> {
    let requested_address = SocketAddr::try_from_raw(raw_address, address_length)?;
    let requested_port = requested_address.port();
    let incoming_config = crate::setup().incoming_config();

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

    // we don't use `is_localhost` here since unspecified means to listen
    // on all IPs.
    if incoming_config.ignore_localhost && requested_address.ip().is_loopback() {
        return Detour::Bypass(Bypass::IgnoreLocalhost(requested_port));
    }

    // To handle #1458, we don't ignore port `0` for UDP.
    if (is_ignored_tcp_port(&requested_address, incoming_config)
        && matches!(socket.kind, SocketKind::Tcp(_)))
        || crate::setup().is_debugger_port(&requested_address)
        || incoming_config.ignore_ports.contains(&requested_port)
    {
        Err(Bypass::Port(requested_address.port()))?;
    }

    // Check that the domain matches the requested address.
    let domain_valid = match socket.domain {
        libc::AF_INET => requested_address.is_ipv4(),
        libc::AF_INET6 => requested_address.is_ipv6(),
        _ => false,
    };
    if !domain_valid {
        Err(HookError::InvalidBindAddressForDomain)?;
    }

    // Check if the user's requested address isn't already in use, even though it's not actually
    // bound, as we bind to a different address, but if we don't check for this then we're
    // changing normal socket behavior (see issue #1123).
    if SOCKETS
        .lock()?
        .iter()
        .any(|(_, socket)| match &socket.state {
            SocketState::Initialized | SocketState::Connected(_) => false,
            SocketState::Bound(bound) | SocketState::Listening(bound) => {
                bound.requested_address == requested_address
            }
        })
    {
        Err(HookError::AddressAlreadyBound(requested_address))?;
    }

    // Try to bind a port from listen ports, if no configuration
    // try to bind the requested port, if not available get a random port
    // if there's configuration and binding fails with the requested port
    // we return address not available and not fallback to a random port.
    let listen_port = incoming_config
        .listen_ports
        .get_by_left(&requested_address.port())
        .copied();
    if let Some(port) = listen_port {
        // Listen port was specified. If we fail to bind, we should fail the whole operation.
        bind_similar_address(sockfd, &SocketAddr::new(requested_address.ip(), port))
    } else {
        // Listen port was not specified. If we fail to bind, it's ok to fall back to a random port.
        bind_similar_address(sockfd, &requested_address).or_else(|error| {
            trace!(%error, %requested_address, "bind failed, trying to bind to on a random port");
            bind_similar_address(sockfd, &SocketAddr::new(requested_address.ip(), 0))
        })
    }?;

    // We need to find out what's the port we bound to, that'll be used by `poll_agent` to
    // connect to.
    let address = unsafe {
        SockAddr::try_init(|storage, len| {
            if FN_GETSOCKNAME(sockfd, storage.cast(), len) == -1 {
                let error = io::Error::last_os_error();
                error!(%error, sockfd, "`getsockname` failed to get address of a socket after bind");
                Err(error)
            } else {
                Ok(())
            }
        })
    }
    .ok()
    .and_then(|(_, address)| address.as_socket())
    .bypass(Bypass::AddressConversion)?;

    Arc::get_mut(&mut socket).unwrap().state = SocketState::Bound(Bound {
        requested_address,
        address,
    });

    SOCKETS.lock()?.insert(sockfd, socket);

    // node reads errno to check if bind was successful and doesn't care about the return value
    // (???)
    errno::set_errno(errno::Errno(0));
    Detour::Success(0)
}

/// Subscribe to the agent on the real port. Messages received from the agent on the real port will
/// later be routed to the fake local port.
#[mirrord_layer_macro::instrument(level = Level::TRACE, fields(pid = std::process::id()), ret)]
pub(super) fn listen(sockfd: RawFd, backlog: c_int) -> Detour<i32> {
    let mut socket = {
        SOCKETS
            .lock()?
            .remove(&sockfd)
            .bypass(Bypass::LocalFdNotFound(sockfd))?
    };

    let setup = crate::setup();

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
        SocketState::Bound(Bound {
            requested_address,
            address,
        }) => {
            let listen_result = unsafe { FN_LISTEN(sockfd, backlog) };
            if listen_result != 0 {
                let error = io::Error::last_os_error();

                error!("listen -> Failed `listen` sockfd {:#?}", sockfd);

                Err(error)?
            }

            let mapped_port = setup
                .incoming_config()
                .port_mapping
                .get_by_left(&requested_address.port())
                .copied()
                .unwrap_or_else(|| requested_address.port());

            common::make_proxy_request_with_response(PortSubscribe {
                listening_on: address,
                subscription: setup.incoming_mode().subscription(mapped_port),
            })??;

            // this log message is expected by some E2E tests
            tracing::debug!("daemon subscribed port {}", requested_address.port());

            Arc::get_mut(&mut socket).unwrap().state = SocketState::Listening(Bound {
                requested_address,
                address,
            });

            SOCKETS.lock()?.insert(sockfd, socket);

            Detour::Success(listen_result)
        }
        _ => Detour::Bypass(Bypass::InvalidState(sockfd)),
    }
}

/// Common logic between Tcp/Udp `connect`, when used for the outgoing traffic feature.
///
/// Sends a hook message that will be handled by `(Tcp|Udp)OutgoingHandler`, starting the request
/// interception procedure.
/// This returns errno so we can restore the correct errno in case result is -1 (until we get
/// back to the hook we might call functions that will corrupt errno)
#[mirrord_layer_macro::instrument(level = "trace", ret)]
fn connect_outgoing<const CALL_CONNECT: bool>(
    sockfd: RawFd,
    remote_address: SockAddr,
    mut user_socket_info: Arc<UserSocket>,
    protocol: NetProtocol,
) -> Detour<ConnectResult> {
    // Closure that performs the connection with mirrord messaging.
    let remote_connection = |remote_address: SockAddr| {
        // Prepare this socket to be intercepted.
        let remote_address = SocketAddress::try_from(remote_address).unwrap();

        let request = OutgoingConnectRequest {
            remote_address: remote_address.clone(),
            protocol,
        };
        let response = common::make_proxy_request_with_response(request)??;

        let OutgoingConnectResponse {
            layer_address,
            in_cluster_address,
        } = response;

        // Connect to the interceptor socket that is listening.
        let connect_result: ConnectResult = if CALL_CONNECT {
            let layer_address = SockAddr::try_from(layer_address.clone())?;

            unsafe { FN_CONNECT(sockfd, layer_address.as_ptr(), layer_address.len()) }.into()
        } else {
            ConnectResult {
                result: 0,
                error: None,
            }
        };

        if connect_result.is_failure() {
            error!(
                "connect -> Failed call to libc::connect with {:#?}",
                connect_result,
            );
            Err(io::Error::last_os_error())?
        }

        let connected = Connected {
            remote_address,
            local_address: in_cluster_address,
            layer_address: Some(layer_address),
        };

        trace!("we are connected {connected:#?}");

        Arc::get_mut(&mut user_socket_info).unwrap().state = SocketState::Connected(connected);
        SOCKETS.lock()?.insert(sockfd, user_socket_info);

        Detour::Success(connect_result)
    };

    if remote_address.is_unix() {
        let connect_result = remote_connection(remote_address)?;
        Detour::Success(connect_result)
    } else {
        // Can't just connect to whatever `remote_address` is, as it might be a remotely resolved
        // address, in a local connection context (or vice-versa), so we let `remote_connection`
        // handle this address trickery.
        match crate::setup()
            .outgoing_selector()
            .get_connection_through(remote_address.as_socket()?, protocol)?
        {
            ConnectionThrough::Remote(addr) => {
                let connect_result = remote_connection(SockAddr::from(addr))?;
                Detour::Success(connect_result)
            }
            ConnectionThrough::Local(addr) => {
                let rawish_local_addr = SockAddr::from(addr);

                let connect_result = ConnectResult::from(unsafe {
                    FN_CONNECT(sockfd, rawish_local_addr.as_ptr(), rawish_local_addr.len())
                });

                Detour::Success(connect_result)
            }
        }
    }
}

/// Iterate through sockets, if any of them has the requested port that the application is now
/// trying to connect to - then don't forward this connection to the agent, and instead of
/// connecting to the requested address, connect to the actual address where the application
/// is locally listening.
#[mirrord_layer_macro::instrument(level = "trace", ret)]
fn connect_to_local_address(
    sockfd: RawFd,
    user_socket_info: &UserSocket,
    ip_address: SocketAddr,
) -> Detour<Option<ConnectResult>> {
    if crate::setup().outgoing_config().ignore_localhost {
        Detour::Bypass(Bypass::IgnoreLocalhost(ip_address.port()))
    } else {
        Detour::Success(
            SOCKETS
                .lock()?
                .iter()
                .find_map(|(_, socket)| match socket.state {
                    SocketState::Listening(Bound {
                        requested_address,
                        address,
                    }) => (requested_address.port() == ip_address.port()
                        && socket.protocol == user_socket_info.protocol)
                        .then(|| SockAddr::from(address)),
                    _ => None,
                })
                .map(|rawish_remote_address| unsafe {
                    FN_CONNECT(
                        sockfd,
                        rawish_remote_address.as_ptr(),
                        rawish_remote_address.len(),
                    )
                })
                .map(|connect_result| {
                    if connect_result != 0 {
                        Detour::Error::<ConnectResult>(io::Error::last_os_error().into())
                    } else {
                        Detour::Success(connect_result.into())
                    }
                })
                .transpose()?,
        )
    }
}

/// Handles 3 different cases, depending if the outgoing traffic feature is enabled or not:
///
/// 1. Outgoing traffic is **disabled**: this just becomes a normal `libc::connect` call, removing
/// the socket from our list of managed sockets.
///
/// 2. Outgoing traffic is **enabled** and `socket.state` is `Initialized`: sends a hook message
/// that will be handled by `(Tcp|Udp)OutgoingHandler`, starting the request interception procedure.
///
/// 3. `sockt.state` is `Bound`: part of the tcp mirror feature.
#[mirrord_layer_macro::instrument(level = "trace", ret, skip(raw_address))]
pub(super) fn connect(
    sockfd: RawFd,
    raw_address: *const sockaddr,
    address_length: socklen_t,
) -> Detour<ConnectResult> {
    let remote_address = SockAddr::try_from_raw(raw_address, address_length)?;
    let optional_ip_address = remote_address.as_socket();

    let unix_streams = crate::setup().remote_unix_streams();

    trace!("in connect {:#?}", SOCKETS);

    let user_socket_info = match SOCKETS.lock()?.remove(&sockfd) {
        Some(socket) => socket,
        None => {
            // Socket was probably removed from `SOCKETS` in `bind` detour (as not interesting in
            // terms of `incoming` feature).
            // Here we just recreate `UserSocket` using domain and type fetched from the descriptor
            // we have.
            let domain = nix::sys::socket::getsockname::<SockaddrStorage>(sockfd)
                .map_err(io::Error::from)?
                .family()
                .map(|family| family as i32)
                .unwrap_or(-1);
            if domain != libc::AF_INET && domain != libc::AF_UNIX {
                return Detour::Bypass(Bypass::Domain(domain));
            }
            // I really hate it, but nix seems to really make this API bad :()
            let borrowed_fd = unsafe { BorrowedFd::borrow_raw(sockfd) };
            let type_ = nix::sys::socket::getsockopt(&borrowed_fd, sockopt::SockType)
                .map_err(io::Error::from)? as i32;
            let kind = SocketKind::try_from(type_)?;

            Arc::new(UserSocket::new(domain, type_, 0, Default::default(), kind))
        }
    };

    if let Some(ip_address) = optional_ip_address {
        if crate::setup().experimental().tcp_ping4_mock && ip_address.port() == 7 {
            let connect_result = ConnectResult {
                result: 0,
                error: None,
            };
            return Detour::Success(connect_result);
        }

        let ip = ip_address.ip();
        if ip.is_loopback() || ip.is_unspecified() {
            if let Some(result) = connect_to_local_address(sockfd, &user_socket_info, ip_address)? {
                // `result` here is always a success, as error and bypass are returned on the `?`
                // above.
                return Detour::Success(result);
            }
        }

        if is_ignored_port(&ip_address) || crate::setup().is_debugger_port(&ip_address) {
            return Detour::Bypass(Bypass::Port(ip_address.port()));
        }
    } else if remote_address.is_unix() {
        let address = remote_address
            .as_pathname()
            .map(|path| path.to_string_lossy())
            .or(remote_address
                .as_abstract_namespace()
                .map(String::from_utf8_lossy))
            .map(String::from);

        let handle_remotely = address
            .as_ref()
            .map(|addr| unix_streams.is_match(addr))
            .unwrap_or(false);
        if !handle_remotely {
            return Detour::Bypass(Bypass::UnixSocket(address));
        }
    } else {
        // We do not hijack connections of this address type - Bypass!
        return Detour::Bypass(Bypass::Domain(remote_address.domain().into()));
    }

    let enabled_tcp_outgoing = crate::setup().outgoing_config().tcp;
    let enabled_udp_outgoing = crate::setup().outgoing_config().udp;

    match NetProtocol::from(user_socket_info.kind) {
        NetProtocol::Datagrams if enabled_udp_outgoing => connect_outgoing::<true>(
            sockfd,
            remote_address,
            user_socket_info,
            NetProtocol::Datagrams,
        ),

        NetProtocol::Stream => match user_socket_info.state {
            SocketState::Initialized | SocketState::Bound(..)
                if (optional_ip_address.is_some() && enabled_tcp_outgoing)
                    || (remote_address.is_unix() && !unix_streams.is_empty()) =>
            {
                connect_outgoing::<true>(
                    sockfd,
                    remote_address,
                    user_socket_info,
                    NetProtocol::Stream,
                )
            }

            _ => Detour::Bypass(Bypass::DisabledOutgoing),
        },

        _ => Detour::Bypass(Bypass::DisabledOutgoing),
    }
}

/// Resolve fake local address to real remote address. (IP & port of incoming traffic on the
/// cluster)
#[mirrord_layer_macro::instrument(level = Level::TRACE, skip(address, address_len))]
pub(super) fn getpeername(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> Detour<i32> {
    let remote_address = {
        SOCKETS
            .lock()?
            .get(&sockfd)
            .bypass(Bypass::LocalFdNotFound(sockfd))
            .and_then(|socket| match &socket.state {
                SocketState::Connected(connected) => {
                    Detour::Success(connected.remote_address.clone())
                }
                _ => Detour::Bypass(Bypass::InvalidState(sockfd)),
            })?
    };

    trace!("getpeername -> remote_address {:#?}", remote_address);

    fill_address(address, address_len, remote_address.try_into()?)
}

/// Resolve the fake local address to the real local address.
///
/// ## Port `0` special
///
/// When [`libc::bind`]ing on port `0`, we change the port to be the actual bound port, as this is
/// consistent behavior with libc.
#[mirrord_layer_macro::instrument(level = Level::TRACE, ret, skip(address, address_len))]
pub(super) fn getsockname(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> Detour<i32> {
    let local_address = {
        SOCKETS
            .lock()?
            .get(&sockfd)
            .bypass(Bypass::LocalFdNotFound(sockfd))
            .and_then(|socket| match &socket.state {
                SocketState::Connected(connected) => {
                    Detour::Success(connected.local_address.clone())
                }
                SocketState::Bound(Bound {
                    requested_address,
                    address,
                }) => {
                    if requested_address.port() == 0 {
                        Detour::Success(
                            SocketAddr::new(requested_address.ip(), address.port()).into(),
                        )
                    } else {
                        Detour::Success((*requested_address).into())
                    }
                }
                SocketState::Listening(bound) => Detour::Success(bound.requested_address.into()),
                _ => Detour::Bypass(Bypass::InvalidState(sockfd)),
            })?
    };

    trace!("getsockname -> local_address {:#?}", local_address);

    fill_address(address, address_len, local_address.try_into()?)
}

/// When the fd is "ours", we accept and use [`ConnMetadataRequest`] to retrieve peer address from
/// the internal proxy.
#[mirrord_layer_macro::instrument(level = "trace", ret, skip(address, address_len))]
pub(super) fn accept(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    new_fd: RawFd,
) -> Detour<RawFd> {
    let (domain, protocol, type_, port, listener_address) = {
        SOCKETS
            .lock()?
            .get(&sockfd)
            .bypass(Bypass::LocalFdNotFound(sockfd))
            .and_then(|socket| match &socket.state {
                SocketState::Listening(Bound {
                    requested_address,
                    address,
                }) => Detour::Success((
                    socket.domain,
                    socket.protocol,
                    socket.type_,
                    requested_address.port(),
                    *address,
                )),
                _ => Detour::Bypass(Bypass::InvalidState(sockfd)),
            })?
    };

    let peer_address = {
        let stream = unsafe { TcpStream::from_raw_fd(new_fd) };
        let peer_address = stream.peer_addr();
        let _fd = stream.into_raw_fd();
        peer_address?
    };

    let ConnMetadataResponse {
        remote_source,
        local_address,
    } = common::make_proxy_request_with_response(ConnMetadataRequest {
        listener_address,
        peer_address,
    })?;

    let state = SocketState::Connected(Connected {
        remote_address: remote_source.into(),
        local_address: SocketAddr::new(local_address, port).into(),
        layer_address: None,
    });

    let new_socket = UserSocket::new(domain, type_, protocol, state, type_.try_into()?);

    fill_address(address, address_len, remote_source.into())?;

    SOCKETS.lock()?.insert(new_fd, Arc::new(new_socket));

    Detour::Success(new_fd)
}

#[mirrord_layer_macro::instrument(level = "trace")]
pub(super) fn fcntl(orig_fd: c_int, cmd: c_int, fcntl_fd: i32) -> Result<(), HookError> {
    match cmd {
        libc::F_DUPFD | libc::F_DUPFD_CLOEXEC => dup::<true>(orig_fd, fcntl_fd),
        _ => Ok(()),
    }
}

/// Managed part of our [`dup_detour`], that clones the `Arc<T>` thing we have keyed by `fd`
/// ([`UserSocket`], or [`file::ops::RemoteFile`]).
///
/// - `SWITCH_MAP`:
///
/// Indicates that we're switching the `fd` from [`SOCKETS`] to [`OPEN_FILES`] (or vice-versa).
///
/// We need this to properly handle some cases in [`fcntl`], [`dup2_detour`], and [`dup3_detour`].
/// Extra relevant for node on macos.
#[mirrord_layer_macro::instrument(level = "trace", ret)]
pub(super) fn dup<const SWITCH_MAP: bool>(fd: c_int, dup_fd: i32) -> Result<(), HookError> {
    let mut sockets = SOCKETS.lock()?;
    if let Some(socket) = sockets.get(&fd).cloned() {
        sockets.insert(dup_fd as RawFd, socket);

        if SWITCH_MAP {
            OPEN_FILES.lock()?.remove(&dup_fd);
        }

        return Ok(());
    }

    let mut open_files = OPEN_FILES.lock()?;
    if let Some(file) = open_files.get(&fd) {
        let cloned_file = file.clone();
        open_files.insert(dup_fd as RawFd, cloned_file);

        if SWITCH_MAP {
            sockets.remove(&dup_fd);
        }
    }

    Ok(())
}

/// Handles the remote communication part of [`getaddrinfo`], call this if you want to resolve a DNS
/// through the agent, but don't need to deal with all the [`libc::getaddrinfo`] stuff.
///
/// # Note
///
/// This function updates the mapping in [`REMOTE_DNS_REVERSE_MAPPING`].
#[mirrord_layer_macro::instrument(level = "trace", ret)]
pub(super) fn remote_getaddrinfo(node: String) -> HookResult<Vec<(String, IpAddr)>> {
    let addr_info_list = common::make_proxy_request_with_response(GetAddrInfoRequest { node })?.0?;

    let mut remote_dns_reverse_mapping = REMOTE_DNS_REVERSE_MAPPING.lock()?;
    addr_info_list.iter().for_each(|lookup| {
        remote_dns_reverse_mapping.insert(lookup.ip, lookup.name.clone());
    });

    Ok(addr_info_list
        .into_iter()
        .map(|LookupRecord { name, ip }| (name, ip))
        .collect())
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
#[mirrord_layer_macro::instrument(level = "trace", ret)]
pub(super) fn getaddrinfo(
    rawish_node: Option<&CStr>,
    rawish_service: Option<&CStr>,
    raw_hints: Option<&libc::addrinfo>,
) -> Detour<*mut libc::addrinfo> {
    let node: String = rawish_node
        .bypass(Bypass::NullNode)?
        .to_str()
        .map_err(|fail| {
            warn!(
                "Failed converting `rawish_node` from `CStr` with {:#?}",
                fail
            );

            Bypass::CStrConversion
        })?
        .into();

    // Convert `service` to port
    let service = rawish_service
        .map(CStr::to_str)
        .transpose()
        .map_err(|fail| {
            warn!(
                "Failed converting `raw_service` from `CStr` with {:#?}",
                fail
            );

            Bypass::CStrConversion
        })?
        .and_then(|service| service.parse::<u16>().ok())
        .unwrap_or(0);

    crate::setup().dns_selector().check_query(&node, service)?;

    let raw_hints = raw_hints
        .cloned()
        .unwrap_or_else(|| unsafe { mem::zeroed() });

    // TODO(alex): Use more fields from `raw_hints` to respect the user's `getaddrinfo` call.
    let libc::addrinfo {
        ai_socktype,
        ai_protocol,
        ..
    } = raw_hints;

    // Some apps (gRPC on Python) use `::` to listen on all interfaces, and usually that just means
    // resolve on unspecified. So we just return that in IpV4 because we don't support ipv6.
    let resolved_addr = if node == "::" {
        // name is "" because that's what happens in real flow.
        vec![("".to_string(), IpAddr::V4(Ipv4Addr::UNSPECIFIED))]
    } else {
        remote_getaddrinfo(node.clone())?
    };

    let mut managed_addr_info = MANAGED_ADDRINFO.lock()?;
    // Only care about: `ai_family`, `ai_socktype`, `ai_protocol`.
    let result = resolved_addr
        .into_iter()
        .map(|(name, address)| {
            let rawish_sock_addr = SockAddr::from(SocketAddr::new(address, service));
            let ai_addrlen = rawish_sock_addr.len();
            let ai_family = rawish_sock_addr.family() as _;

            // Must outlive this function, as it is stored as a pointer in `libc::addrinfo`.
            let ai_addr = Box::into_raw(Box::new(unsafe { *rawish_sock_addr.as_ptr() }));
            let ai_canonname = CString::new(name).unwrap().into_raw();

            libc::addrinfo {
                ai_flags: 0,
                ai_family,
                ai_socktype,
                // TODO(alex): Don't just reuse whatever the user passed to us.
                ai_protocol,
                ai_addrlen,
                ai_addr,
                ai_canonname,
                ai_next: ptr::null_mut(),
            }
        })
        .rev()
        .map(Box::new)
        .map(Box::into_raw)
        .map(|raw| {
            managed_addr_info.insert(raw as usize);
            raw
        })
        .reduce(|current, previous| {
            // Safety: These pointers were just allocated using `Box::new`, so they should be
            // fine regarding memory layout, and are not dangling.
            unsafe { (*previous).ai_next = current };
            previous
        })
        .ok_or(HookError::DNSNoName)?;

    trace!("getaddrinfo -> result {:#?}", result);

    Detour::Success(result)
}

/// Retrieves the `hostname` from the agent's `/etc/hostname` to be used by [`gethostname`]
fn remote_hostname_string() -> Detour<CString> {
    if crate::setup().local_hostname() {
        Detour::Bypass(Bypass::LocalHostname)?;
    }

    let hostname_path = PathBuf::from("/etc/hostname");

    let OpenFileResponse { fd } = file::ops::RemoteFile::remote_open(
        hostname_path,
        OpenOptionsInternal {
            read: true,
            ..Default::default()
        },
    )?;

    let ReadFileResponse { bytes, read_amount } = file::ops::RemoteFile::remote_read(fd, 256)?;

    let _ = file::ops::RemoteFile::remote_close(fd).inspect_err(|fail| {
        trace!("Leaking remote file fd (should be harmless) due to {fail:#?}!")
    });

    CString::new(
        bytes
            .into_iter()
            .take(read_amount as usize - 1)
            .collect::<Vec<_>>(),
    )
    .map(Detour::Success)?
}

/// Resolves a hostname and set result to static global like the original `gethostbyname` does.
///
/// Used by erlang/elixir to resolve DNS.
///
/// **Safety**:
/// See the [`GETHOSTBYNAME_ALIASES_PTR`] docs. If you see this function being called and some weird
/// issue is going on, assume that you might've triggered the UB.
#[mirrord_layer_macro::instrument(level = "trace", ret)]
pub(super) fn gethostbyname(raw_name: Option<&CStr>) -> Detour<*mut hostent> {
    let name: String = raw_name
        .bypass(Bypass::NullNode)?
        .to_str()
        .map_err(|fail| {
            warn!("Failed converting `name` from `CStr` with {:#?}", fail);

            Bypass::CStrConversion
        })?
        .into();

    crate::setup().dns_selector().check_query(&name, 0)?;

    let hosts_and_ips = remote_getaddrinfo(name.clone())?;

    // We could `unwrap` here, as this would have failed on the previous conversion.
    let host_name = CString::new(name)?;

    if hosts_and_ips.is_empty() {
        set_errno(errno::Errno(libc::EAI_NODATA));
        return Detour::Success(ptr::null_mut());
    }

    // We need `*mut _` at the end, so `ips` has to be `mut`.
    let (aliases, mut ips) = hosts_and_ips
        .into_iter()
        .filter_map(|(host, ip)| match ip {
            // Only care about ipv4s and hosts that exist.
            IpAddr::V4(ip) => {
                let c_host = CString::new(host).ok()?;
                Some((c_host, ip.octets()))
            }
            IpAddr::V6(ip) => {
                trace!("ipv6 received - ignoring - {ip:?}");
                None
            }
        })
        .fold(
            (Vec::default(), Vec::default()),
            |(mut aliases, mut ips), (host, octets)| {
                aliases.push(host);
                ips.push(octets);
                (aliases, ips)
            },
        );

    let mut aliases_ptrs: Vec<*const i8> = aliases
        .iter()
        .map(|alias| alias.as_ptr().cast())
        .collect::<Vec<_>>();
    let mut ips_ptrs = ips.iter_mut().map(|ip| ip.as_mut_ptr()).collect::<Vec<_>>();

    // Put a null ptr to signal end of the list.
    aliases_ptrs.push(ptr::null());
    ips_ptrs.push(ptr::null_mut());

    // Need long-lived values so we can take pointers to them.
    unsafe {
        GETHOSTBYNAME_HOSTNAME.replace(host_name);
        GETHOSTBYNAME_ALIASES_STR.replace(aliases);
        GETHOSTBYNAME_ALIASES_PTR.replace(aliases_ptrs);
        GETHOSTBYNAME_ADDRESSES_VAL.replace(ips);
        GETHOSTBYNAME_ADDRESSES_PTR.replace(ips_ptrs);

        // Fill the `*mut hostent` that the user will interact with.
        GETHOSTBYNAME_HOSTENT.h_name = GETHOSTBYNAME_HOSTNAME.as_ref().unwrap().as_ptr() as _;
        GETHOSTBYNAME_HOSTENT.h_length = 4;
        GETHOSTBYNAME_HOSTENT.h_addrtype = libc::AF_INET;
        GETHOSTBYNAME_HOSTENT.h_aliases = GETHOSTBYNAME_ALIASES_PTR.as_ref().unwrap().as_ptr() as _;
        GETHOSTBYNAME_HOSTENT.h_addr_list =
            GETHOSTBYNAME_ADDRESSES_PTR.as_ref().unwrap().as_ptr() as *mut *mut libc::c_char;
    }

    Detour::Success(unsafe { std::ptr::addr_of!(GETHOSTBYNAME_HOSTENT) as _ })
}

/// Resolve hostname from remote host with caching for the result
#[mirrord_layer_macro::instrument(level = "trace")]
pub(super) fn gethostname() -> Detour<&'static CString> {
    HOSTNAME.get_or_detour_init(remote_hostname_string)
}

/// ## DNS resolution on port `53`
///
/// We handle UDP sockets by putting them in a sort of _semantically_ connected state, meaning that
/// we don't actually [`libc::connect`] `sockfd`, but we change the respective [`UserSocket`] to
/// [`SocketState::Connected`].
///
/// Here we check for this invariant, so if a user calls [`libc::recvmsg`] without previously
/// either connecting this `sockfd`, or calling [`libc::sendto`], we're going to panic.
///
/// It performs the [`fill_address`] requirement of [`libc::recvmsg`] with the correct remote
/// address (instead of using our interceptor address).
///
/// ## Any other port
///
/// When this function is called, we've already ran [`libc::recvfrom`], and checked for a successful
/// result from it, so `raw_source` has been pre-filled for us, but it might contain the wrong
/// address, if the packet came from one of our modified (connected) sockets.
///
/// If the socket was bound to `0.0.0.0:{not 53}`, then `raw_source` here should be
/// `127.0.0.1:{not 53}`, keeping in mind that the sender socket remains with address
/// `0.0.0.0:{not 53}` (same behavior as not using mirrord).
///
/// When the socket is in a [`Connected`] state, we call [`fill_address`] with its `remote_address`,
/// instead of letting whatever came in `raw_source` through.
///
/// See [`send_to`] for more information.
#[mirrord_layer_macro::instrument(level = "trace", ret, skip(raw_source, source_length))]
pub(super) fn recv_from(
    sockfd: i32,
    recv_from_result: isize,
    raw_source: *mut sockaddr,
    source_length: *mut socklen_t,
) -> Detour<isize> {
    SOCKETS
        .lock()?
        .get(&sockfd)
        .and_then(|socket| match &socket.state {
            SocketState::Connected(Connected { remote_address, .. }) => {
                Some(remote_address.clone())
            }
            _ => None,
        })
        .map(SocketAddress::try_into)?
        .map(|address| fill_address(raw_source, source_length, address))??;

    errno::set_errno(errno::Errno(0));
    Detour::Success(recv_from_result)
}

/// Helps manually resolving DNS on port `53` with UDP, see [`send_to`] and [`sendmsg`].
#[mirrord_layer_macro::instrument(level = "trace", ret)]
fn send_dns_patch(
    sockfd: RawFd,
    user_socket_info: Arc<UserSocket>,
    destination: SocketAddr,
) -> Detour<SockAddr> {
    let mut sockets = SOCKETS.lock()?;
    // We want to keep holding this socket.
    sockets.insert(sockfd, user_socket_info);

    // Sending a packet on port NOT 53.
    let destination = sockets
        .iter()
        .filter(|(_, socket)| socket.kind.is_udp())
        // Is the `destination` one of our sockets? If so, then we grab the actual address,
        // instead of the, possibly fake address from mirrord.
        .find_map(|(_, receiver_socket)| match &receiver_socket.state {
            SocketState::Bound(Bound {
                requested_address,
                address,
            }) => {
                // Special case for port `0`, see `getsockname`.
                if requested_address.port() == 0 {
                    (SocketAddr::new(requested_address.ip(), address.port()) == destination)
                        .then_some(*address)
                } else {
                    (*requested_address == destination).then_some(*address)
                }
            }
            SocketState::Connected(Connected {
                remote_address,
                layer_address,
                ..
            }) => {
                let remote_address: SocketAddr = remote_address.clone().try_into().ok()?;
                let layer_address: SocketAddr = layer_address.clone()?.try_into().ok()?;

                if remote_address == destination {
                    Some(layer_address)
                } else {
                    None
                }
            }
            _ => None,
        })?;

    Detour::Success(SockAddr::from(destination))
}

/// ## DNS resolution on port `53`
///
/// There is a bit of trickery going on here, as this function first triggers a _semantical_
/// connection to an interceptor socket (we don't [`libc::connect`] this `sockfd`, just change the
/// [`UserSocket`] state), and only then calls the actual [`libc::sendto`] to send `raw_message` to
/// the interceptor address (instead of the `raw_destination`).
///
/// ## Any other port
///
/// When `destination.port != 53`, we search our [`SOCKETS`] to see if we're holding the
/// `destination` address, which would have the receiving socket bound to a different address than
/// what the user sees.
///
/// If we find `destination` as the `requested_address` of one of our [`Bound`] sockets, then we
/// [`libc::sendto`] to the bound `address`. A similar logic applies to a [`Connected`] socket.
///
/// ## Destination is `0.0.0.0:{not 53}`
///
/// No special care is taken here, sending a packet to this address behaves the same with or without
/// mirrord, which is: destination becomes `127.0.0.1:{not 53}`.
///
/// See [`recv_from`] for more information.
#[mirrord_layer_macro::instrument(
    level = "trace",
    ret,
    skip(raw_message, raw_destination, destination_length)
)]
pub(super) fn send_to(
    sockfd: RawFd,
    raw_message: *const c_void,
    message_length: usize,
    flags: i32,
    raw_destination: *const sockaddr,
    destination_length: socklen_t,
) -> Detour<isize> {
    let destination = SockAddr::try_from_raw(raw_destination, destination_length)?;
    trace!("destination {:?}", destination.as_socket());

    let user_socket_info = SOCKETS
        .lock()?
        .remove(&sockfd)
        .ok_or(Bypass::LocalFdNotFound(sockfd))?;

    // we don't support unix sockets which don't use `connect`
    if (destination.is_unix() || user_socket_info.domain == AF_UNIX)
        && !matches!(user_socket_info.state, SocketState::Connected(_))
    {
        return Detour::Bypass(Bypass::Domain(AF_UNIX));
    }

    // Currently this flow only handles DNS resolution.
    // So here we have to check for 2 things:
    //
    // 1. Are we sending something port 53? Then we use mirrord flow;
    // 2. Is the destination a socket that we have bound? Then we send it to the real address that
    // we've bound the destination socket.
    //
    // If none of the above are true, then the destination is some real address outside our scope.
    let sent_result = if let Some(destination) = destination
        .as_socket()
        .filter(|destination| destination.port() != 53)
    {
        let rawish_true_destination = send_dns_patch(sockfd, user_socket_info, destination)?;

        unsafe {
            FN_SEND_TO(
                sockfd,
                raw_message,
                message_length,
                flags,
                rawish_true_destination.as_ptr(),
                rawish_true_destination.len(),
            )
        }
    } else {
        connect_outgoing::<false>(
            sockfd,
            destination,
            user_socket_info,
            NetProtocol::Datagrams,
        )?;

        let layer_address: SockAddr = SOCKETS
            .lock()?
            .get(&sockfd)
            .and_then(|socket| match &socket.state {
                SocketState::Connected(connected) => connected.layer_address.clone(),
                _ => unreachable!(),
            })
            .map(SocketAddress::try_into)??;

        let raw_interceptor_address = layer_address.as_ptr();
        let raw_interceptor_length = layer_address.len();

        unsafe {
            FN_SEND_TO(
                sockfd,
                raw_message,
                message_length,
                flags,
                raw_interceptor_address,
                raw_interceptor_length,
            )
        }
    };

    Detour::Success(sent_result)
}

/// Same behavior as [`send_to`], the only difference is that here we deal with [`libc::msghdr`],
/// instead of directly with socket addresses.
#[mirrord_layer_macro::instrument(level = "trace", ret, skip(raw_message_header))]
pub(super) fn sendmsg(
    sockfd: RawFd,
    raw_message_header: *const libc::msghdr,
    flags: i32,
) -> Detour<isize> {
    // We have a destination, so apply our fake `connect` patch.
    let destination = (!unsafe { *raw_message_header }.msg_name.is_null()).then(|| {
        let raw_destination = unsafe { *raw_message_header }.msg_name as *const libc::sockaddr;
        let destination_length = unsafe { *raw_message_header }.msg_namelen;
        SockAddr::try_from_raw(raw_destination, destination_length)
    })??;

    trace!("destination {:?}", destination.as_socket());

    // send_dns_patch acquires lock, so don't hold it
    let user_socket_info = SOCKETS
        .lock()?
        .remove(&sockfd)
        .ok_or(Bypass::LocalFdNotFound(sockfd))?;

    // we don't support unix sockets which don't use `connect`
    if (destination.is_unix() || user_socket_info.domain == AF_UNIX)
        && !matches!(user_socket_info.state, SocketState::Connected(_))
    {
        return Detour::Bypass(Bypass::Domain(AF_UNIX));
    }

    // Currently this flow only handles DNS resolution.
    // So here we have to check for 2 things:
    //
    // 1. Are we sending something port 53? Then we use mirrord flow;
    // 2. Is the destination a socket that we have bound? Then we send it to the real address that
    // we've bound the destination socket.
    //
    // If none of the above are true, then the destination is some real address outside our scope.
    let sent_result = if let Some(destination) = destination
        .as_socket()
        .filter(|destination| destination.port() != 53)
    {
        let rawish_true_destination = send_dns_patch(sockfd, user_socket_info, destination)?;

        let mut true_message_header = Box::new(unsafe { *raw_message_header });

        unsafe {
            true_message_header
                .as_mut()
                .msg_name
                .copy_from_nonoverlapping(
                    rawish_true_destination.as_ptr() as *const _,
                    rawish_true_destination.len() as usize,
                )
        };
        true_message_header.as_mut().msg_namelen = rawish_true_destination.len();

        unsafe { FN_SENDMSG(sockfd, true_message_header.as_ref(), flags) }
    } else {
        connect_outgoing::<false>(
            sockfd,
            destination,
            user_socket_info,
            NetProtocol::Datagrams,
        )?;

        let layer_address: SockAddr = SOCKETS
            .lock()?
            .get(&sockfd)
            .and_then(|socket| match &socket.state {
                SocketState::Connected(connected) => connected.layer_address.clone(),
                _ => unreachable!(),
            })
            .map(SocketAddress::try_into)??;

        let raw_interceptor_address = layer_address.as_ptr() as *const _;
        let raw_interceptor_length = layer_address.len();
        let mut true_message_header = Box::new(unsafe { *raw_message_header });

        unsafe {
            true_message_header
                .as_mut()
                .msg_name
                .copy_from_nonoverlapping(raw_interceptor_address, raw_interceptor_length as usize)
        };
        true_message_header.as_mut().msg_namelen = raw_interceptor_length;

        unsafe { FN_SENDMSG(sockfd, true_message_header.as_ref(), flags) }
    };

    Detour::Success(sent_result)
}
