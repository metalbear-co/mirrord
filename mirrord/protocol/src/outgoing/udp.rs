use super::*;
use crate::RemoteResult;

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum LayerUdpOutgoing {
    /// User is interested in connecting via udp to some remote address, specified in
    /// [`LayerConnect`].
    ///
    /// The layer will get a mirrord managed address that it'll `connect` to, meanwhile
    /// in the agent we `connect` to the actual remote address.
    ///
    /// Saying that we have an _udp connection_ is a bit weird, considering it's a
    /// _connectionless_ protocol, but in mirrord we use a _fakeish_ connection mechanism
    /// when dealing with outgoing udp traffic.
    Connect(LayerConnect),

    /// Write data to the remote address the agent is `connect`ed to.
    ///
    /// There's no `Read` message, as we're calling `read` in the agent, and we send
    /// a [`DaemonUdpOutgoing::Read`] message in case we get some data from this connection.
    Write(LayerWrite),

    /// The layer closed the connection, this message syncs up the agent, closing it
    /// over there as well.
    ///
    /// Connections in the agent may be closed in other ways, such as when an error happens
    /// when reading or writing. Which means that this message is not the only way of
    /// closing outgoing udp connections.
    Close(LayerClose),
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum DaemonUdpOutgoing {
    /// The agent attempted a connection to the remote address specified by
    /// [`LayerUdpOutgoing::Connect`], and it might've been successful or not.
    ///
    /// See the docs for [`LayerUdpOutgoing::Connect`] for a bit more information on the
    /// weird idea of `connect` and udp in mirrord.
    Connect(RemoteResult<DaemonConnect>),

    /// Read data from the connection.
    ///
    /// There's no `Write` message, as `write`s come from the user (layer). The agent sending
    /// a `write` to the layer like this would make no sense, since it could just `write` it
    /// to the remote connection itself.
    Read(RemoteResult<DaemonRead>),

    /// Tell the layer that this connection has been `close`d, either by a request from
    /// the user with [`LayerUdpOutgoing::Close`], or from some error in the agent when
    /// writing or reading from the connection.
    Close(ConnectionId),
}
