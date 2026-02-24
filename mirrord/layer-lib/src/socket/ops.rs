use std::{
    convert::TryFrom,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    ops::Not,
    sync::Arc,
};
#[cfg(unix)]
use std::{
    io,
    os::{fd::BorrowedFd, unix::io::RawFd},
};

#[cfg(unix)]
use libc::AF_UNIX;
use mirrord_intproxy_protocol::{NetProtocol, OutgoingConnectRequest, OutgoingConnectResponse};
use mirrord_protocol::outgoing::SocketAddress;
#[cfg(unix)]
use nix::sys::socket::{SockaddrStorage, sockopt};
use socket2::SockAddr;
#[cfg(unix)]
use tracing::error;
use tracing::{debug, trace};
/// Platform-specific connect function types
#[cfg(windows)]
use winapi::um::winsock2::{WSA_IO_PENDING, WSAEINPROGRESS, WSAEINTR};

use super::sockets::socket_descriptor_to_i64;
#[cfg(windows)]
use crate::socket::dns::windows::check_address_reachability;
use crate::{
    HookError, HookResult,
    detour::Bypass,
    error::ConnectError,
    proxy_connection::make_proxy_request_with_response,
    setup::setup,
    socket::{
        Bound, Connected, ConnectionThrough, SOCKETS, SocketDescriptor, SocketState, UserSocket,
        sockets::reconstruct_user_socket,
    },
};
#[cfg(windows)]
use crate::{error::windows::WindowsError, socket::sockets::find_listener_address_by_port};

/// Result type for connect operations that preserves errno information
#[derive(Debug)]
pub struct ConnectResult {
    result: i32,
    error: Option<i32>,
}

impl ConnectResult {
    pub fn new(result: i32, error: Option<i32>) -> Self {
        Self { result, error }
    }

    pub fn is_failure(&self) -> bool {
        self.error.is_some_and(|error| {
            #[cfg(unix)]
            {
                error != libc::EINTR && error != libc::EINPROGRESS
            }
            #[cfg(windows)]
            {
                error != WSAEINTR && error != WSAEINPROGRESS && error != WSA_IO_PENDING
            }
        })
    }

    pub fn result(&self) -> i32 {
        self.result
    }

    pub fn error(&self) -> Option<i32> {
        self.error
    }
}

impl From<i32> for ConnectResult {
    fn from(result: i32) -> Self {
        if result == -1 {
            ConnectResult {
                result,
                error: Some(get_last_error()),
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
            set_last_error(error);
        }
        connect_result.result
    }
}

// Platform-specific error handling
#[cfg(unix)]
fn errno_location() -> *mut libc::c_int {
    unsafe {
        #[cfg(target_os = "macos")]
        {
            libc::__error()
        }
        #[cfg(not(target_os = "macos"))]
        {
            libc::__errno_location()
        }
    }
}

fn get_last_error() -> i32 {
    #[cfg(unix)]
    unsafe {
        *errno_location()
    }

    #[cfg(windows)]
    match WindowsError::wsa_last_error() {
        WindowsError::WinSock(code) => code,
        _ => unreachable!(),
    }
}

fn set_last_error(error: i32) {
    #[cfg(unix)]
    unsafe {
        *errno_location() = error
    };

    #[cfg(windows)]
    unsafe {
        winapi::um::winsock2::WSASetLastError(error)
    };
}

pub fn nop_connect_fn(_addr: SockAddr) -> ConnectResult {
    ConnectResult {
        result: 0,
        error: None,
    }
}

/// Iterate through sockets, if any of them has the requested port that the application is now
/// trying to connect to - then don't forward this connection to the agent, and instead of
/// connecting to the requested address, connect to the actual address where the application
/// is locally listening.
#[cfg(unix)]
#[mirrord_layer_macro::instrument(level = "trace", skip(connect_fn), ret)]
pub fn connect_to_local_address<F>(
    sockfd: RawFd,
    user_socket_info: &UserSocket,
    ip_address: SocketAddr,
    connect_fn: F,
) -> HookResult<Option<ConnectResult>>
where
    F: FnOnce(SockAddr) -> ConnectResult,
{
    if setup().outgoing_config().ignore_localhost {
        Err(HookError::Bypass(Bypass::IgnoredInIncoming(ip_address)))
    } else {
        Ok(SOCKETS
            .lock()?
            .iter()
            .find_map(|(_, socket)| match socket.state {
                SocketState::Listening(Bound {
                    requested_address,
                    address,
                }) => (requested_address.port() == ip_address.port()
                    && socket.protocol == user_socket_info.protocol)
                    .then(|| SockAddr::from(address)),
                SocketState::Bound { .. }
                | SocketState::Initialized
                | SocketState::Connected(_) => None,
            })
            .map(connect_fn))
    }
}

/// Helper function to check if a port should be ignored (port 0)
#[inline]
pub fn is_ignored_port(addr: &SocketAddr) -> bool {
    addr.port() == 0
}

/// Handles 3 different cases, depending on if the outgoing traffic feature is enabled or not:
///
/// 1. Outgoing traffic is **disabled**: this just becomes a normal `libc::connect` call, removing
/// the socket from our list of managed sockets.
///
/// 2. Outgoing traffic is **enabled** and `socket.state` is `Initialized`: sends a hook message
/// that will be handled by `(Tcp|Udp)OutgoingHandler`, starting the request interception procedure.
///
/// 3. `sockt.state` is `Bound`: part of the tcp mirror feature.
#[allow(clippy::result_large_err)]
#[mirrord_layer_macro::instrument(level = "trace", skip(connect_fn), ret)]
pub fn connect_common<F>(
    sockfd: SocketDescriptor,
    remote_address: SockAddr,
    connect_fn: F,
) -> HookResult<ConnectResult>
where
    F: FnOnce(SockAddr) -> ConnectResult,
{
    let user_socket_info = match SOCKETS.lock()?.remove(&sockfd) {
        Some(socket) => socket,
        None => reconstruct_user_socket(sockfd)?,
    };

    let optional_ip_address = remote_address.as_socket();
    let unix_streams = setup().remote_unix_streams();

    let mut connect_fn = Some(connect_fn);
    let mut call_connect_fn = |addr: SockAddr| -> ConnectResult {
        connect_fn
            .take()
            .expect("connect_fn should only be called once")(addr)
    };

    if let Some(ip_address) = optional_ip_address {
        if setup().experimental().tcp_ping4_mock && ip_address.port() == 7 {
            return Ok(ConnectResult::from(0));
        }

        // Handle localhost/unspecified addresses first -
        //  if applicable, connect locally without proxy
        let ip = ip_address.ip();
        if ip.is_loopback() || ip.is_unspecified() {
            // Note(Daniel): the next 2 if blocks are logically equivalent but were brought from
            // layer and layer-win separately, they work and im only doing plumbing
            // unification, so i rather keep both and each platform will run its own flavoring of
            // it.
            #[cfg(unix)]
            if let Some(result) = connect_to_local_address(
                sockfd,
                &user_socket_info,
                ip_address,
                &mut call_connect_fn,
            )? {
                // `result` here is always a success, as error and bypass are returned on the `?`
                // above.
                return Ok(result);
            }
            #[cfg(windows)]
            if let Some(local_address) =
                find_listener_address_by_port(ip_address.port(), user_socket_info.protocol)
            {
                tracing::debug!("connecting locally to listener at {}", local_address);
                let local_sockaddr = SockAddr::from(local_address);
                let connect_result = call_connect_fn(local_sockaddr);
                return Ok(connect_result);
            }
        }

        if is_ignored_port(&ip_address) {
            return Err(HookError::Bypass(Bypass::IgnoredInIncoming(ip_address)));
        }

        // Ports 50000 and 50001 are commonly used to communicate with sidecar containers.
        let bypass_debugger_check = ip_address.port() == 50000 || ip_address.port() == 50001;
        if bypass_debugger_check.not() && setup().is_debugger_port(&ip_address) {
            return Err(HookError::Bypass(Bypass::IgnoredInIncoming(ip_address)));
        }
    } else if remote_address.is_unix() {
        #[cfg(unix)]
        {
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
                return Err(HookError::Bypass(Bypass::UnixSocket(address)));
            }
        }
        #[cfg(windows)]
        {
            debug!("bypassing unix socket, not supported on windows");
            return Err(HookError::Bypass(Bypass::UnixSocket(None)));
        }
    } else {
        // We do not hijack connections of this address type - Bypass!
        return Err(HookError::Bypass(Bypass::Domain(
            remote_address.domain().into(),
        )));
    }

    tracing::info!("intercepting connection to {:?}", remote_address);

    let enabled_tcp_outgoing = setup().outgoing_config().tcp;
    let enabled_udp_outgoing = setup().outgoing_config().udp;

    match NetProtocol::from(user_socket_info.kind) {
        NetProtocol::Datagrams if enabled_udp_outgoing => connect_outgoing_common(
            sockfd,
            remote_address,
            user_socket_info,
            NetProtocol::Datagrams,
            &mut call_connect_fn,
        ),

        NetProtocol::Stream => match user_socket_info.state {
            SocketState::Initialized | SocketState::Bound { .. }
                if (optional_ip_address.is_some() && enabled_tcp_outgoing)
                    || (remote_address.is_unix() && !unix_streams.is_empty()) =>
            {
                connect_outgoing_common(
                    sockfd,
                    remote_address,
                    user_socket_info,
                    NetProtocol::Stream,
                    call_connect_fn,
                )
            }

            _ => Err(HookError::Bypass(Bypass::DisabledOutgoing)),
        },

        _ => Err(HookError::Bypass(Bypass::DisabledOutgoing)),
    }
}

/// Common logic for outgoing connections that can be used by platform-specific implementations.
///
/// This function handles the mirrord proxy connection setup but leaves the actual socket
/// connection to the platform-specific implementation via the `connect_fn` callback.
///
/// ## Parameters
/// - `sockfd`: The socket file descriptor
/// - `remote_address`: The address the user wants to connect to
/// - `user_socket_info`: The socket information structure
/// - `protocol`: The network protocol (TCP/UDP)
/// - `connect_fn`: Platform-specific function to perform the actual socket connection
///
/// ## Returns
/// A `ConnectResult` containing the connection result and any error information
#[mirrord_layer_macro::instrument(level = "debug", ret, skip(user_socket_info, connect_fn))]
pub fn connect_outgoing_common<F>(
    sockfd: SocketDescriptor,
    remote_address: SockAddr,
    mut user_socket_info: Arc<UserSocket>,
    protocol: NetProtocol,
    connect_fn: F,
) -> HookResult<ConnectResult>
where
    F: FnOnce(SockAddr) -> ConnectResult,
{
    debug!("preparing {protocol:?} connection to {remote_address:?}");

    let mut connect_fn = Some(connect_fn);
    let mut call_connect_fn = |addr: SockAddr| -> ConnectResult {
        connect_fn
            .take()
            .expect("connect_fn should only be called once")(addr)
    };

    // Closure that performs the connection with mirrord messaging.
    let remote_connection = |remote_addr: SockAddr| -> HookResult<ConnectResult> {
        // Prepare this socket to be intercepted.
        let remote_address = SocketAddress::try_from(remote_addr.clone()).unwrap();

        let request = OutgoingConnectRequest {
            remote_address: remote_address.clone(),
            protocol,
        };

        let response = match make_proxy_request_with_response(request)? {
            Ok(response) => response,
            Err(e) => return Err(ConnectError::ProxyRequest(format!("{:?}", e)).into()),
        };

        let OutgoingConnectResponse {
            connection_id,
            mut layer_address,
            in_cluster_address,
        } = response;

        if let SocketAddress::Ip(interceptor_addr) = &mut layer_address {
            // Our socket can be bound to any local interface,
            // so the interceptor listens on an unspecified IP address, e.g. 0.0.0.0
            // We need to fill the exact IP here.
            match &user_socket_info.state {
                SocketState::Bound {
                    bound: Bound { address, .. },
                    ..
                } => {
                    if interceptor_addr.ip().is_unspecified() {
                        if interceptor_addr.is_ipv4() {
                            interceptor_addr.set_ip(Ipv4Addr::LOCALHOST.into())
                        } else {
                            interceptor_addr.set_ip(Ipv6Addr::LOCALHOST.into())
                        }
                    } else {
                        interceptor_addr.set_ip(address.ip());
                    }
                }
                _ if interceptor_addr.is_ipv4() => {
                    interceptor_addr.set_ip(Ipv4Addr::LOCALHOST.into())
                }
                _ => interceptor_addr.set_ip(Ipv6Addr::LOCALHOST.into()),
            }
        }

        // Connect to the socket prepared by the internal proxy.
        let layer_addr = SockAddr::try_from(layer_address.clone())?;
        let connect_result: ConnectResult = call_connect_fn(layer_addr);

        #[cfg(unix)]
        if let Some(raw_error) = connect_result.error
            && raw_error != libc::EINTR
            && raw_error != libc::EINPROGRESS
        {
            // We failed to connect to the internal proxy socket.
            // This is most likely a bug,
            // so let's try and include here as much info as possible.

            let error = io::Error::from_raw_os_error(raw_error);
            let borrowed_fd = unsafe { BorrowedFd::borrow_raw(sockfd) };
            let socket_name =
                nix::sys::socket::getsockname::<SockaddrStorage>(sockfd).map(|sockaddr| {
                    // `Debug` implementation is not very helpful, `ToString` produces a nice
                    // address.
                    sockaddr.to_string()
                });
            let socket_type = nix::sys::socket::getsockopt(&borrowed_fd, sockopt::SockType);
            error!(
                sockfd,
                ?user_socket_info,
                ?socket_type,
                socket_addr = ?socket_name,

                outgoing_connect_request_id = connection_id,
                internal_proxy_socket_address = %layer_address,
                agent_peer_address = %remote_address,
                agent_local_address = in_cluster_address.as_ref().map(ToString::to_string),

                raw_error,
                %error,

                ?SOCKETS,

                "Failed to connect to an internal proxy socket. \
                This is most likely a bug, please report it.",
            );
            return Err(error.into());
        }

        let connected = Connected {
            connection_id: Some(connection_id),
            remote_address,
            local_address: in_cluster_address,
            layer_address: Some(layer_address),
        };

        trace!("we are connected {connected:#?}");

        Arc::get_mut(&mut user_socket_info).unwrap().state = SocketState::Connected(connected);
        SOCKETS.lock()?.insert(sockfd, user_socket_info);
        Ok(connect_result)
    };

    let connect_result = if remote_address.is_unix() {
        remote_connection(remote_address)?
    } else {
        // make sure its a valid IPV4/6 address
        let socket_addr = remote_address
            .as_socket()
            .ok_or(HookError::UnsupportedSocketType)?;

        // Can't just connect to whatever `remote_address` is, as it might be a remotely resolved
        // address, in a local connection context (or vice versa), so we let `remote_connection`
        // handle this address trickery.
        match setup()
            .outgoing_selector()
            .get_connection_through(socket_addr, protocol)?
        {
            ConnectionThrough::Remote(addr) => remote_connection(SockAddr::from(addr))?,
            ConnectionThrough::Local(addr) => {
                #[cfg(windows)]
                if check_address_reachability(sockfd, &addr) != 0 {
                    tracing::error!("socket {} connect target {:?} is unreachable", sockfd, addr);
                    return Err(ConnectError::AddressUnreachable(format!("{}", addr)).into());
                }

                call_connect_fn(SockAddr::from(addr))
            }
        }
    };
    Ok(connect_result)
}

/// Creates an outgoing connection request for the specified address and protocol
pub fn create_outgoing_request(
    remote_address: SocketAddr,
    protocol: NetProtocol,
) -> OutgoingConnectRequest {
    OutgoingConnectRequest {
        remote_address: remote_address.into(),
        protocol,
    }
}

/// Checks if an address should be handled as a Unix socket
pub fn is_unix_address(address: &SockAddr) -> bool {
    address.is_unix()
}

/// Converts a socket address to the appropriate format for outgoing connections
pub fn prepare_outgoing_address(address: SocketAddr) -> SocketAddress {
    address.into()
}

/// Helps manually resolving DNS on port `53` with UDP, see `send_to` and `sendmsg`.
///
/// This function is used for DNS resolution handling and socket address mapping. When sending
/// UDP packets to non-port-53 destinations, it checks if the destination is one of our bound
/// sockets and returns the actual address if found.
///
/// ## Parameters
/// - `sockfd`: The socket file descriptor
/// - `user_socket_info`: Information about the socket making the request
/// - `destination`: The destination address being sent to
///
/// ## Returns
/// The actual socket address to send to, which may be different from the requested destination
/// if the destination is one of our managed sockets.
#[mirrord_layer_macro::instrument(level = "trace", ret, skip(user_socket_info))]
pub fn send_dns_patch(
    sockfd: SocketDescriptor,
    user_socket_info: Arc<UserSocket>,
    destination: SocketAddr,
) -> HookResult<SockAddr> {
    let mut sockets = SOCKETS.lock()?;
    // We want to keep holding this socket.
    sockets.insert(sockfd, user_socket_info);

    // Sending a packet on port NOT 53.
    let destination = sockets
        .iter()
        .filter(|(_, socket)| socket.kind.is_udp())
        // Is the `destination` one of our sockets? If so, then we grab the actual address,
        // instead of the possibly fake address from mirrord.
        .find_map(|(_, receiver_socket)| match &receiver_socket.state {
            SocketState::Bound {
                bound:
                    Bound {
                        requested_address,
                        address,
                    },
                ..
            } => {
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
        })
        .ok_or(HookError::ManagedSocketNotFound(destination))?;

    Ok(destination.into())
}

/// ## DNS resolution on port `53`
///
/// There is a bit of trickery going on here, as this function first triggers a _semantical_
/// connection to an interceptor socket (we don't `connect` this `sockfd`, just change the
/// [`UserSocket`] state), and only then calls the actual `sendto` to send `raw_message` to
/// the interceptor address (instead of the `raw_destination`).
///
/// Once the [`UserSocket`] is in the [`Connected`] state, then any other `sendto` call with
/// a [`None`] `raw_destination` will call the original `sendto` to the bound `address`. A
/// similar logic applies to a `Bound` socket.
///
/// ## Usage
///
/// This function preserves the architecture and comments from the original Unix layer
/// implementation, providing a cross-platform interface for UDP sendto operations that
/// supports both DNS resolution (port 53) and regular UDP packet sending.
///
/// ## Parameters
/// - `sockfd`: Socket file descriptor
/// - `raw_message`: Pointer to the message buffer
/// - `message_length`: Length of the message
/// - `flags`: Send flags
/// - `raw_destination`: Pointer to destination address
/// - `sendto_fn`: Platform-specific sendto function
///
/// ## Returns
/// `HookResult<isize>`
#[mirrord_layer_macro::instrument(level = "debug", ret, skip(raw_message, destination, sendto_fn))]
pub fn send_to<F>(
    sockfd: SocketDescriptor,
    raw_message: *const u8,
    message_length: usize,
    flags: i32,
    destination: SocketAddr,
    sendto_fn: F,
) -> HookResult<isize>
where
    F: FnOnce(SocketDescriptor, *const u8, usize, i32, SockAddr) -> HookResult<isize>,
{
    let raw_destination = SockAddr::from(destination);
    trace!("destination {:?}", destination);

    let user_socket_info = SOCKETS
        .lock()?
        .remove(&sockfd)
        .ok_or_else(|| HookError::SocketNotFound(socket_descriptor_to_i64(sockfd)))?;

    // we don't support unix sockets which don't use `connect`
    #[cfg(unix)]
    if (raw_destination.is_unix() || user_socket_info.domain == AF_UNIX)
        && !matches!(user_socket_info.state, SocketState::Connected(_))
    {
        return Err(HookError::UnsupportedSocketType);
    }

    // Currently this flow only handles DNS resolution.
    // So here we have to check for 2 things:
    //
    // 1. Are we sending something port 53? Then we use mirrord flow;
    // 2. Is the destination a socket that we have bound? Then we send it to the real address that
    // we've bound the destination socket.
    //
    // If none of the above are true, then the destination is some real address outside our scope.
    if let Some(destination) = raw_destination
        .as_socket()
        .filter(|destination| destination.port() != 53)
    {
        debug!(
            "layer-lib send_to -> non-DNS destination (port {}), checking for DNS patch",
            destination.port()
        );
        let rawish_true_destination = send_dns_patch(sockfd, user_socket_info, destination)?;

        sendto_fn(
            sockfd,
            raw_message,
            message_length,
            flags,
            rawish_true_destination,
        )
    } else {
        connect_outgoing_common(
            sockfd,
            destination.into(),
            user_socket_info,
            NetProtocol::Datagrams,
            nop_connect_fn,
        )?;

        let layer_address: SockAddr = SOCKETS
            .lock()?
            .get(&sockfd)
            .and_then(|socket| match &socket.state {
                SocketState::Connected(connected) => connected.layer_address.clone(),
                _ => unreachable!(),
            })
            .ok_or(HookError::ManagedSocketNotFound(destination))?
            .try_into()?;

        // Convert Windows parameters to cross-platform format
        sendto_fn(sockfd, raw_message, message_length, flags, layer_address)
    }
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr};

    use super::*;

    #[test]
    fn test_connect_result_success() {
        let result = ConnectResult::from(0);
        assert!(!result.is_failure());
        assert_eq!(result.result(), 0);
        assert_eq!(result.error(), None);
    }

    #[test]
    fn test_connect_result_failure() {
        let result = ConnectResult::new(-1, Some(1)); // Generic error code
        assert!(result.is_failure());
        assert_eq!(result.result(), -1);
        assert_eq!(result.error(), Some(1));
    }

    #[test]
    fn test_create_outgoing_request() {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 8080);
        let request = create_outgoing_request(addr, NetProtocol::Stream);

        assert_eq!(request.protocol, NetProtocol::Stream);
        // Additional assertions would depend on SocketAddress implementation
    }

    #[test]
    fn test_is_unix_address() {
        // This test would need a Unix socket address to be meaningful
        // For now, just test that the function exists and compiles
        let addr = SockAddr::from(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            8080,
        ));
        assert!(!is_unix_address(&addr));
    }
}
