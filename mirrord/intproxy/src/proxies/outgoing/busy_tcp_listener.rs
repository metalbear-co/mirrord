use std::{
    io,
    net::{Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

use futures::{FutureExt, TryFutureExt, future::join_all};
use strum::IntoEnumIterator;
use strum_macros::EnumIter;
use tokio::{
    net::{TcpListener, TcpSocket, TcpStream},
    sync::OnceCell,
};

static WORKING_METHOD: OnceCell<Option<BusyListenerMethod>> = OnceCell::const_new();

/// Method of creating a [`BusyTcpListener`].
///
/// Since all known method are essentially hacks, we have no guarantee that they'll work.
/// When selecting a method at runtime, use [`Self::recommended`].
#[derive(Clone, Copy, Debug, EnumIter)]
pub enum BusyListenerMethod {
    /// Create and bind a TCP socket, but do not call `listen`.
    /// Call `listen` and `accept` only when ready to accept the connection.
    ///
    /// Observed to work on macOS.
    NoListen,
    /// Create and bind a TCP socket. Call `listen` with backlog size 0.
    /// Make a TCP connection to the socket, but do not `accept` it.
    /// The connection should succeed immediately, and fill the backlog queue.
    ///
    /// When accepting, first close the dummy connection. Then, accept connections until the peer
    /// address is different than the one used for the dummy connection.
    ///
    /// Observed to work on Linux.
    Backlog0,
    /// Same as [`BusyListenerMethod::Backlog0`], but with backlog size 1.
    ///
    /// Observed to work on macOS.
    Backlog1,
}

impl BusyListenerMethod {
    /// Returns a [`BusyListenerMethod`] that works on this system (if any).
    ///
    /// On the first call, this function might take some time to complete, as it performs tests of
    /// all known methods. Subsequent calls should return instantly.
    pub async fn recommended() -> Option<Self> {
        *WORKING_METHOD
            .get_or_init(|| async {
                let futs = BusyListenerMethod::iter().map(|method| {
                    tokio::time::timeout(Duration::from_secs(2), method.check_if_works())
                        .unwrap_or_else(|_elapsed| {
                            Err(io::Error::other("busy listener method check timed out"))
                        })
                        .map(move |result| (method, result))
                });
                let results = join_all(futs).await;
                results.iter().for_each(|(method, result)| {
                    tracing::debug!(?result, "Busy listener method {method:?} checked",);
                });
                results
                    .into_iter()
                    .filter_map(|(method, result)| result.is_ok().then_some(method))
                    .next()
                    .inspect(|method| {
                        tracing::info!("Selected busy listener method {method:?}");
                    })
            })
            .await
    }

    /// Verifies if this method works on this system.
    ///
    /// Note that this is a best effort check.
    ///
    /// # Returns
    ///
    /// If the method fails the check, returns [`Err`].
    async fn check_if_works(self) -> io::Result<()> {
        let prepared = self.prepare_socket(true).await?;
        let addr = prepared.local_addr()?;

        let mut conn_fut = Box::pin(TcpStream::connect(addr));

        // The connect future should not complete until we call `.accept()` on the prepared socket.
        // We don't have any reliable way to verify this, so we gotta do what we can.
        // Here, we expect the future not to complete within 300ms.
        // If the localhost connect does not finish within 300ms, our method is probably working.
        //
        // In a real life scenario, outgoing connect request was observed to take ~250ms.
        // Hence, we use 300ms here.
        match tokio::time::timeout(Duration::from_millis(300), &mut conn_fut).await {
            Err(_elapsed) => {}
            Ok(stream) => {
                stream?;
                return Err(io::Error::other("connect attempt was not blocked"));
            }
        }

        tokio::try_join!(prepared.accept(), conn_fut)?;

        Ok(())
    }

    /// Creates a new [`BusyTcpListener`].
    pub async fn prepare_socket(self, ipv4: bool) -> io::Result<BusyTcpListener> {
        let bind_to = if ipv4 {
            SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 0)
        } else {
            SocketAddr::new(Ipv6Addr::UNSPECIFIED.into(), 0)
        };
        let socket = if ipv4 {
            TcpSocket::new_v4()?
        } else {
            TcpSocket::new_v6()?
        };
        socket.bind(bind_to)?;

        match self {
            Self::NoListen => Ok(BusyTcpListener::Socket(socket)),
            Self::Backlog0 => {
                let listener = socket.listen(0)?;
                let dummy_self_conn = TcpStream::connect(listener.local_addr()?).await?;
                Ok(BusyTcpListener::Listener {
                    listener,
                    dummy_self_conn,
                })
            }
            Self::Backlog1 => {
                let listener = socket.listen(1)?;
                let dummy_self_conn = TcpStream::connect(listener.local_addr()?).await?;
                Ok(BusyTcpListener::Listener {
                    listener,
                    dummy_self_conn,
                })
            }
        }
    }
}

/// Localhost TCP listener, always seen by other sockets as "busy".
///
/// In other words, all attempts to connect to this listener will hang,
/// until you explicitly call [`BusyTcpListener::accept`].
///
/// Note that this is not the case with plain [`TcpListener`]s:
///
/// ```
/// use tokio::net::{TcpListener, TcpStream};
///
/// #[tokio::main(flavor = "current_thread")]
/// async fn main() {
///     let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
///     let connection = TcpStream::connect(listener.local_addr().unwrap())
///         .await
///         .unwrap();
///     println!("Connect finished without calling .accept() on the listener!");
/// }
/// ```
///
/// This is because the kernel automatically handles accepting TCP connections.
/// In order to prevent this, we use one of [`BusyListenerMethod`]s.
///
/// # Why?
///
/// This trick is a part of the non-blocking TCP connect flow.
/// See [`OutgoingProxy`](super::OutgoingProxy) doc for reference.
#[derive(Debug)]
pub enum BusyTcpListener {
    Socket(TcpSocket),
    Listener {
        listener: TcpListener,
        dummy_self_conn: TcpStream,
    },
}

impl BusyTcpListener {
    /// Accepts a TCP connection on this listener.
    pub async fn accept(self) -> io::Result<TcpStream> {
        match self {
            Self::Socket(socket) => {
                let listener = socket.listen(1)?;
                let (stream, _) = listener.accept().await?;
                Ok(stream)
            }
            Self::Listener {
                listener,
                dummy_self_conn,
            } => {
                let dummy_addr = dummy_self_conn.local_addr()?;
                std::mem::drop(dummy_self_conn);
                loop {
                    let (stream, addr) = listener.accept().await?;
                    if addr != dummy_addr {
                        break Ok(stream);
                    }
                }
            }
        }
    }

    /// Returns the local address of the listener.
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        match self {
            Self::Socket(socket) => socket.local_addr(),
            Self::Listener { listener, .. } => listener.local_addr(),
        }
    }
}

#[cfg(test)]
mod test {
    use super::BusyListenerMethod;

    /// Verifies that any of [`BusyListenerMethod`] works on this system.
    #[tokio::test]
    async fn any_method_works() {
        assert!(BusyListenerMethod::recommended().await.is_some());
    }
}
