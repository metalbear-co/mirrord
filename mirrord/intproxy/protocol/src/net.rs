use std::{ffi::c_int, net::SocketAddr};

use bincode::{Decode, Encode};
use mirrord_protocol::outgoing::SocketAddress;

// TODO(alex): We could treat `sockfd` as being the same as `&self` for socket ops, we currently
// can't do that due to how `dup` interacts directly with our `Arc<UserSocket>`, because we just
// `clone` the arc, we end up with exact duplicates, but `dup` generates a new fd that we have no
// way of putting inside the duplicated `UserSocket`.
#[derive(Encode, Decode, Debug, Clone)]
#[allow(dead_code)]
pub struct UserSocket {
    pub domain: c_int,
    pub type_: c_int,
    pub protocol: c_int,
    pub state: SocketState,
    pub kind: SocketKind,
}

#[derive(Encode, Decode, Debug, Clone, Copy, PartialEq, Eq)]
pub enum SocketKind {
    Tcp(c_int),
    Udp(c_int),
}

#[derive(Encode, Decode, Debug, Default, Clone)]
pub enum SocketState {
    #[default]
    Initialized,
    Bound(Bound),
    Listening(Bound),
    Connected(Connected),
}

/// Contains the addresses of a mirrord connected socket.
///
/// - `layer_address` is only used for the outgoing feature.
#[derive(Encode, Decode, Debug, Clone)]
pub struct Connected {
    /// The address requested by the user that we're "connected" to.
    ///
    /// Whenever the user calls [`libc::getpeername`], this is the address we return to them.
    ///
    /// For the _outgoing_ feature, we actually connect to the `layer_address` interceptor socket,
    /// but use this address in the [`libc::recvfrom`] handling of [`fill_address`].
    pub remote_address: SocketAddress,

    /// Local address (pod-wise)
    ///
    /// ## Example
    ///
    /// ```sh
    /// $ kubectl get pod -o wide
    ///
    /// NAME             READY   STATUS    IP
    /// impersonated-pod 0/1     Running   1.2.3.4
    /// ```
    ///
    /// We would set this ip as `1.2.3.4:{port}` in `bind`, where `{port}` is the user requested
    /// port.
    pub local_address: SocketAddress,

    /// The address of the interceptor socket, this is what we're really connected to in the
    /// outgoing feature.
    pub layer_address: Option<SocketAddress>,
}

/// Represents a [`SocketState`] where the user made a [`libc::bind`] call, and we intercepted it.
///
/// ## Details
///
/// Our [`ops::bind`] hook doesn't bind the address that the user passed to us, instead we call
/// [`hooks::FN_BIND`] with `localhost:0` (or `unspecified:0` for ipv6), and use
/// [`hooks::FN_GETSOCKNAME`] to retrieve this bound address which we assign to `Bound::address`.
///
/// The original user requested address is assigned to `Bound::requested_address`, and used as an
/// illusion for when the user calls [`libc::getsockname`], as if this address was the actual local
/// bound address.
#[derive(Encode, Decode, Debug, Clone, Copy)]
pub struct Bound {
    /// Address originally requested by the user for `bind`.
    pub requested_address: SocketAddr,

    /// Actual bound address that we use to communicate between the user's listener socket and our
    /// interceptor socket.
    pub address: SocketAddr,
}
