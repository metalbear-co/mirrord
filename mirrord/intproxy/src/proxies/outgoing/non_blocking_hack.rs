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

static WORKING_METHOD: OnceCell<Option<HackMethod>> = OnceCell::const_new();

pub async fn working_method() -> Option<HackMethod> {
    *WORKING_METHOD
        .get_or_init(|| async {
            let futs = HackMethod::iter().map(|method| {
                tokio::time::timeout(Duration::from_secs(1), method.test())
                    .unwrap_or_else(|_elapsed| Err(io::Error::other("test timed out")))
                    .map(move |result| (method, result))
            });
            let results = join_all(futs).await;
            results.iter().for_each(|(method, result)| {
                tracing::debug!(?result, "Non blocking hack {method:?} checked",);
            });
            results
                .into_iter()
                .filter_map(|(method, result)| result.is_ok().then_some(method))
                .next()
        })
        .await
}

#[derive(Clone, Copy, Debug, EnumIter)]
pub enum HackMethod {
    NoListen,
    Backlog0,
    Backlog1,
}

impl HackMethod {
    async fn test(self) -> io::Result<()> {
        let prepared = self.prepare_socket(false).await?;
        let addr = prepared.local_addr()?;

        let mut conn_fut = Box::pin(TcpStream::connect(addr));

        match tokio::time::timeout(Duration::from_secs(200), &mut conn_fut).await {
            Err(_elapsed) => {}
            Ok(stream) => {
                stream?;
                return Err(io::Error::other("connect attempt was not blocked"));
            }
        }

        tokio::try_join!(prepared.accept(), conn_fut)?;

        Ok(())
    }

    pub async fn prepare_socket(self, ipv4: bool) -> io::Result<PreparedTcpSocket> {
        let bind_to = if ipv4 {
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0)
        } else {
            SocketAddr::new(Ipv6Addr::LOCALHOST.into(), 0)
        };
        let socket = if ipv4 {
            TcpSocket::new_v4()?
        } else {
            TcpSocket::new_v6()?
        };
        socket.bind(bind_to)?;

        match self {
            Self::NoListen => Ok(PreparedTcpSocket::Socket(socket)),
            Self::Backlog0 => {
                let listener = socket.listen(0)?;
                let dummy_self_conn = TcpStream::connect(listener.local_addr()?).await?;
                Ok(PreparedTcpSocket::Listener {
                    listener,
                    dummy_self_conn,
                })
            }
            Self::Backlog1 => {
                let listener = socket.listen(1)?;
                let dummy_self_conn = TcpStream::connect(listener.local_addr()?).await?;
                Ok(PreparedTcpSocket::Listener {
                    listener,
                    dummy_self_conn,
                })
            }
        }
    }
}

#[derive(Debug)]
pub enum PreparedTcpSocket {
    Socket(TcpSocket),
    Listener {
        listener: TcpListener,
        dummy_self_conn: TcpStream,
    },
}

impl PreparedTcpSocket {
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

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        match self {
            Self::Socket(socket) => socket.local_addr(),
            Self::Listener { listener, .. } => listener.local_addr(),
        }
    }
}

#[cfg(test)]
mod test {
    #[tokio::test]
    async fn any_hack_works() {
        assert!(super::working_method().await.is_some());
    }
}
