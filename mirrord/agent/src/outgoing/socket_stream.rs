use enum_dispatch::enum_dispatch;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpStream, UnixStream},
};

#[enum_dispatch]
pub trait AsyncReadWrite: AsyncRead + AsyncWrite {}

#[enum_dispatch(AsyncReadWrite)]
pub enum SocketStream {
    Ip(TcpStream),
    Unix(UnixStream),
}
