use std::fmt;

use futures::{Sink, Stream};
use mirrord_protocol::{ClientMessage, DaemonMessage};

/// Trait for mirrord-protocol client->server connection provider.
pub trait ProtocolConnector: 'static + Send + Sync + fmt::Debug {
    /// Type of error that can occur when connecting to the server or working with an established
    /// connection.
    type Error: 'static + std::error::Error + Send + Sync;

    /// Type of established connection.
    type Conn: Connection<Self::Error>;

    /// Attempts to connect to the server.
    fn connect(&mut self) -> impl Future<Output = Result<Self::Conn, Self::Error>> + Send;

    /// Returns whether this connector can make another connection attempt.
    fn can_reconnect(&self) -> bool;
}

/// mirrord-protocol client->server connection as [`Stream`] + [`Sink`].
pub trait Connection<E>:
    'static + Send + Unpin + Stream<Item = Result<DaemonMessage, E>> + Sink<ClientMessage, Error = E>
where
    E: 'static + std::error::Error + Send + Sync,
{
}

impl<C, E> Connection<E> for C
where
    C: 'static
        + Send
        + Unpin
        + Stream<Item = Result<DaemonMessage, E>>
        + Sink<ClientMessage, Error = E>,
    E: 'static + std::error::Error + Send + Sync,
{
}
