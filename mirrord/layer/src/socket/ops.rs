use alloc::ffi::CString;
use core::{ffi::CStr, mem};
use std::{
    io,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream},
    os::{fd::IntoRawFd, unix::io::RawFd},
    path::PathBuf,
    ptr,
    sync::{Arc, OnceLock},
};

use libc::{c_int, c_void, sockaddr, socklen_t};
use mirrord_config::feature::network::incoming::IncomingMode;
use mirrord_intproxy::{
    codec::SyncDecoder,
    protocol::{
        IncomingConnectionMetadata, NetProtocol, OutgoingConnectRequest, OutgoingConnectResponse,
        PortSubscribe,
    },
};
use mirrord_protocol::{
    dns::{GetAddrInfoRequest, LookupRecord},
    file::{OpenFileResponse, OpenOptionsInternal, ReadFileResponse},
};
use socket2::SockAddr;
use tracing::{error, info, trace};

use super::{hooks::*, *};
use crate::{
    detour::{Detour, OnceLockExt, OptionDetourExt, OptionExt},
    error::HookError,
    file::{self, OPEN_FILES},
    proxy_connection::ProxyError,
};

/// Holds the pair of [`IpAddr`] with their hostnames, resolved remotely through
/// [`getaddrinfo`].
///
/// Used by [`connect_outgoing`] to retrieve the hostname from the address that the user called
/// [`connect`] with, so we can resolve it locally when neccessary.
pub(super) static REMOTE_DNS_REVERSE_MAPPING: LazyLock<DashMap<IpAddr, String>> =
    LazyLock::new(|| DashMap::with_capacity(8));

/// Hostname initialized from the agent with [`gethostname`].
pub(crate) static HOSTNAME: OnceLock<CString> = OnceLock::new();

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
#[tracing::instrument(level = "trace", ret)]
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

    SOCKETS.insert(socket_fd, Arc::new(new_socket));

    Detour::Success(socket_fd)
}

#[tracing::instrument(level = "trace", ret)]
fn bind_port(sockfd: c_int, domain: c_int, port: u16) -> Detour<()> {
    let address = match domain {
        libc::AF_INET => Ok(SockAddr::from(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
        ))),
        libc::AF_INET6 => Ok(SockAddr::from(SocketAddr::new(
            IpAddr::V6(Ipv6Addr::UNSPECIFIED),
            port,
        ))),
        invalid => Err(Bypass::Domain(invalid)),
    }?;

    let bind_result = unsafe { FN_BIND(sockfd, address.as_ptr(), address.len()) };
    if bind_result != 0 {
        Detour::Error(io::Error::last_os_error().into())
    } else {
        Detour::Success(())
    }
}

/// Check if the socket is managed by us, if it's managed by us and it's not an ignored port,
/// update the socket state.
#[tracing::instrument(level = "trace", ret, skip(raw_address))]
pub(super) fn bind(
    sockfd: c_int,
    raw_address: *const sockaddr,
    address_length: socklen_t,
) -> Detour<i32> {
    let requested_address = SocketAddr::try_from_raw(raw_address, address_length)?;
    let requested_port = requested_address.port();
    let incoming_config = crate::setup().incoming_config();

    let ignore_localhost = incoming_config.ignore_localhost;

    let mut socket = {
        SOCKETS
            .remove(&sockfd)
            .ok_or(Bypass::LocalFdNotFound(sockfd))
            .and_then(|(_, socket)| {
                if !matches!(socket.state, SocketState::Initialized) {
                    Err(Bypass::InvalidState(sockfd))
                } else {
                    Ok(socket)
                }
            })?
    };

    // we don't use `is_localhost` here since unspecified means to listen
    // on all IPs.
    if ignore_localhost && requested_address.ip().is_loopback() {
        return Detour::Bypass(Bypass::IgnoreLocalhost(requested_port));
    }

    // To handle #1458, we don't ignore port `0` for UDP.
    if (is_ignored_port(&requested_address) && matches!(socket.kind, SocketKind::Tcp(_)))
        || crate::setup().is_debugger_port(&requested_address)
        || incoming_config.ignore_ports.contains(&requested_port)
    {
        Err(Bypass::Port(requested_address.port()))?;
    }

    // Check if the user's requested address isn't already in use, even though it's not actually
    // bound, as we bind to a different address, but if we don't check for this then we're
    // changing normal socket behavior (see issue #1123).
    if SOCKETS.iter().any(|socket| match &socket.state {
        SocketState::Initialized | SocketState::Connected(_) => false,
        SocketState::Bound(bound) | SocketState::Listening(bound) => {
            bound.requested_address == requested_address
        }
    }) {
        Err(HookError::AddressAlreadyBound(requested_address))?;
    }

    // Try to bind a port from listen ports, if no configuration
    // try to bind the requested port, if not available get a random port
    // if there's configuration and binding fails with the requested port
    // we return address not available and not fallback to a random port.
    let listen_port = crate::setup()
        .incoming_config()
        .listen_ports
        .get_by_left(&requested_address.port())
        .cloned();

    // Listen port was specified
    if let Some(port) = listen_port {
        bind_port(sockfd, socket.domain, port)
    } else {
        bind_port(sockfd, socket.domain, requested_address.port())
            .or_else(|e| {
                info!("bind -> first `bind` failed on port {:?} with {e:?}, trying to bind to a random port", requested_address.port());
                bind_port(sockfd, socket.domain, 0)
            }
        )
    }?;

    // We need to find out what's the port we bound to, that'll be used by `poll_agent` to
    // connect to.
    let address = unsafe {
        SockAddr::try_init(|storage, len| {
            if FN_GETSOCKNAME(sockfd, storage.cast(), len) == -1 {
                error!("bind -> Failed `getsockname` sockfd {:#?}", sockfd);

                Err(io::Error::last_os_error())?
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

    SOCKETS.insert(sockfd, socket);

    // node reads errno to check if bind was successful and doesn't care about the return value
    // (???)
    errno::set_errno(errno::Errno(0));
    Detour::Success(0)
}

/// Subscribe to the agent on the real port. Messages received from the agent on the real port will
/// later be routed to the fake local port.
#[tracing::instrument(level = "trace", ret)]
pub(super) fn listen(sockfd: RawFd, backlog: c_int) -> Detour<i32> {
    let mut socket = {
        SOCKETS
            .remove(&sockfd)
            .map(|(_, socket)| socket)
            .bypass(Bypass::LocalFdNotFound(sockfd))?
    };

    if matches!(crate::setup().incoming_config().mode, IncomingMode::Off) {
        return Detour::Bypass(Bypass::DisabledIncoming);
    }

    if crate::setup().targetless() {
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

            common::make_proxy_request_with_response(PortSubscribe {
                listening_on: address.into(),
                subscription: crate::setup()
                    .incoming_mode()
                    .subscription(requested_address.port()),
            })??;

            Arc::get_mut(&mut socket).unwrap().state = SocketState::Listening(Bound {
                requested_address,
                address,
            });

            SOCKETS.insert(sockfd, socket);

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
#[tracing::instrument(level = "trace", ret)]
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
        SOCKETS.insert(sockfd, user_socket_info);

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
#[tracing::instrument(level = "trace", ret)]
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
                .iter()
                .find_map(|socket| match socket.state {
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
#[tracing::instrument(level = "trace", ret, skip(raw_address))]
pub(super) fn connect(
    sockfd: RawFd,
    raw_address: *const sockaddr,
    address_length: socklen_t,
) -> Detour<ConnectResult> {
    let remote_address = SockAddr::try_from_raw(raw_address, address_length)?;
    let optional_ip_address = remote_address.as_socket();

    let unix_streams = crate::setup().remote_unix_streams();

    info!("in connect {:#?}", SOCKETS);

    let (_, user_socket_info) = {
        SOCKETS
            .remove(&sockfd)
            .ok_or(Bypass::LocalFdNotFound(sockfd))?
    };

    if let Some(ip_address) = optional_ip_address {
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
            SocketState::Initialized
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

            SocketState::Bound(Bound { address, .. }) => {
                trace!("connect -> SocketState::Bound {:#?}", user_socket_info);

                let address = SockAddr::from(address);
                let bind_result = unsafe { FN_BIND(sockfd, address.as_ptr(), address.len()) };

                if bind_result != 0 {
                    error!(
                        "connect -> Failed to bind socket result {:?}, address: {:?}, sockfd: {:?}!",
                        bind_result, address, sockfd
                    );

                    Err(io::Error::last_os_error())?
                } else {
                    Detour::Bypass(Bypass::MirrorConnect)
                }
            }

            _ => Detour::Bypass(Bypass::DisabledOutgoing),
        },

        _ => Detour::Bypass(Bypass::DisabledOutgoing),
    }
}

/// Resolve fake local address to real remote address. (IP & port of incoming traffic on the
/// cluster)
#[tracing::instrument(level = "trace", skip(address, address_len))]
pub(super) fn getpeername(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> Detour<i32> {
    let remote_address = {
        SOCKETS
            .get(&sockfd)
            .bypass(Bypass::LocalFdNotFound(sockfd))
            .and_then(|entry| match &entry.value().state {
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
#[tracing::instrument(level = "trace", ret, skip(address, address_len))]
pub(super) fn getsockname(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
) -> Detour<i32> {
    let local_address = {
        SOCKETS
            .get(&sockfd)
            .bypass(Bypass::LocalFdNotFound(sockfd))
            .and_then(|entry| match &entry.value().state {
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

/// Prepares a new incoming TCP connection.
/// This *must* be called on every new mirrored/stolen TCP connection.
/// See [`PortSubscribe`].
#[tracing::instrument(level = "trace", ret)]
fn init_incoming_connection(stream: &mut TcpStream) -> Detour<IncomingConnectionMetadata> {
    let mut decoder: SyncDecoder<IncomingConnectionMetadata, &mut TcpStream> =
        SyncDecoder::new(stream);
    let metadata = decoder
        .receive()
        .map_err(ProxyError::CodecError)?
        .ok_or(ProxyError::NoRemoteAddress)?;

    Detour::Success(metadata)
}

/// When the fd is "ours", we accept and recv the first bytes that contain metadata on the
/// connection to be set in our lock.
///
/// This enables us to have a safe way to get "remote" information (remote ip, port, etc).
#[tracing::instrument(level = "trace", ret, skip(address, address_len))]
pub(super) fn accept(
    sockfd: RawFd,
    address: *mut sockaddr,
    address_len: *mut socklen_t,
    mut new_stream: TcpStream,
) -> Detour<RawFd> {
    let (domain, protocol, type_, port) = {
        SOCKETS
            .get(&sockfd)
            .bypass(Bypass::LocalFdNotFound(sockfd))
            .and_then(|socket| match &socket.state {
                SocketState::Listening(Bound {
                    requested_address, ..
                }) => Detour::Success((
                    socket.domain,
                    socket.protocol,
                    socket.type_,
                    requested_address.port(),
                )),
                _ => Detour::Bypass(Bypass::InvalidState(sockfd)),
            })?
    };

    let IncomingConnectionMetadata {
        local_address,
        remote_source,
    } = init_incoming_connection(&mut new_stream)?;

    let state = SocketState::Connected(Connected {
        remote_address: remote_source.into(),
        local_address: SocketAddr::new(local_address, port).into(),
        layer_address: None,
    });

    let new_socket = UserSocket::new(domain, type_, protocol, state, type_.try_into()?);

    fill_address(address, address_len, remote_source.into())?;

    let fd = new_stream.into_raw_fd();
    SOCKETS.insert(fd, Arc::new(new_socket));

    Detour::Success(fd)
}

#[tracing::instrument(level = "trace")]
pub(super) fn fcntl(orig_fd: c_int, cmd: c_int, fcntl_fd: i32) -> Result<(), HookError> {
    match cmd {
        libc::F_DUPFD | libc::F_DUPFD_CLOEXEC => dup::<true>(orig_fd, fcntl_fd),
        _ => Ok(()),
    }
}

/// Managed part of our [`dup_detour`], that clones the `Arc<T>` thing we have keyed by `fd`
/// ([`UserSocket`], or [`RemoteFile`]).
///
/// - `SWITCH_MAP`:
///
/// Indicates that we're switching the `fd` from [`SOCKETS`] to [`OPEN_FILES`] (or vice-versa).
///
/// We need this to properly handle some cases in [`fcntl`], [`dup2_detour`], and [`dup3_detour`].
/// Extra relevant for node on macos.
#[tracing::instrument(level = "trace", ret)]
pub(super) fn dup<const SWITCH_MAP: bool>(fd: c_int, dup_fd: i32) -> Result<(), HookError> {
    if let Some(socket) = SOCKETS.get(&fd).map(|entry| entry.value().clone()) {
        SOCKETS.insert(dup_fd as RawFd, socket);

        if SWITCH_MAP {
            OPEN_FILES.remove(&dup_fd);
        }

        return Ok(());
    }

    if let Some(file) = OPEN_FILES.view(&fd, |_, file| file.clone()) {
        OPEN_FILES.insert(dup_fd as RawFd, file);

        if SWITCH_MAP {
            SOCKETS.remove(&dup_fd);
        }
    }

    Ok(())
}

/// Handles the remote communication part of [`getaddrinfo`], call this if you want to resolve a DNS
/// through the agent, but don't need to deal with all the [`libc::getaddrinfo`] stuff.
#[tracing::instrument(level = "trace", ret)]
pub(super) fn remote_getaddrinfo(node: String) -> HookResult<Vec<(String, IpAddr)>> {
    let addr_info_list = common::make_proxy_request_with_response(GetAddrInfoRequest { node })?.0?;

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
#[tracing::instrument(level = "trace", ret)]
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
        .map(String::from);

    let raw_hints = raw_hints
        .cloned()
        .unwrap_or_else(|| unsafe { mem::zeroed() });

    // TODO(alex): Use more fields from `raw_hints` to respect the user's `getaddrinfo` call.
    let libc::addrinfo {
        ai_socktype,
        ai_protocol,
        ..
    } = raw_hints;

    // Convert `service` into a port.
    let service = service.map_or(0, |s| s.parse().unwrap_or_default());

    // Only care about: `ai_family`, `ai_socktype`, `ai_protocol`.
    let result = remote_getaddrinfo(node.clone())?
        .into_iter()
        .map(|(name, address)| {
            // Cache the resolved hosts to use in the outgoing traffic filter.
            {
                let _ = REMOTE_DNS_REVERSE_MAPPING.insert(address, node.clone());
            }

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
            MANAGED_ADDRINFO.insert(raw as usize);
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

/// Resolve hostname from remote host with caching for the result
#[tracing::instrument(level = "trace")]
pub(super) fn gethostname() -> Detour<&'static CString> {
    HOSTNAME.get_or_detour_init(remote_hostname_string)
}

/// ## DNS resolution on port `53`
///
/// We handle UDP sockets by putting them in a sort of _semantically_ connected state, meaning that
/// we don't actually [`libc::connect`] `sockfd`, but we change the respective [`UserSocket`] to
/// [`SocketState::Connected`].
///
/// Here we check for this invariant, so if a user calls [`libc::recv_from`] without previously
/// either connecting this `sockfd`, or calling [`libc::send_to`], we're going to panic.
///
/// It performs the [`fill_address`] requirement of [`libc::recv_from`] with the correct remote
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
#[tracing::instrument(level = "trace", ret, skip(raw_source, source_length))]
pub(super) fn recv_from(
    sockfd: i32,
    recv_from_result: isize,
    raw_source: *mut sockaddr,
    source_length: *mut socklen_t,
) -> Detour<isize> {
    SOCKETS
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
#[tracing::instrument(level = "trace", ret)]
fn send_dns_patch(
    sockfd: RawFd,
    user_socket_info: Arc<UserSocket>,
    destination: SocketAddr,
) -> Detour<SockAddr> {
    // We want to keep holding this socket.
    SOCKETS.insert(sockfd, user_socket_info);

    // Sending a packet on port NOT 53.
    let destination = SOCKETS
        .iter()
        .filter(|socket| socket.kind.is_udp())
        // Is the `destination` one of our sockets? If so, then we grab the actual address,
        // instead of the, possibly fake address from mirrord.
        .find_map(|receiver_socket| match &receiver_socket.state {
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
/// [`UserSocket`] state), and only then calls the actual [`libc::send_to`] to send `raw_message` to
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
#[tracing::instrument(
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

    let (_, user_socket_info) = SOCKETS
        .remove(&sockfd)
        .ok_or(Bypass::LocalFdNotFound(sockfd))?;

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
#[tracing::instrument(level = "trace", ret, skip(raw_message_header))]
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

    let (_, user_socket_info) = SOCKETS
        .remove(&sockfd)
        .ok_or(Bypass::LocalFdNotFound(sockfd))?;

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
            true_message_header.as_mut().msg_name.copy_from(
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
                .copy_from(raw_interceptor_address, raw_interceptor_length as usize)
        };
        true_message_header.as_mut().msg_namelen = raw_interceptor_length;

        unsafe { FN_SENDMSG(sockfd, true_message_header.as_ref(), flags) }
    };

    Detour::Success(sent_result)
}
