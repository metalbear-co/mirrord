#![warn(clippy::indexing_slicing)]

use std::env;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};

#[tokio::main]
async fn main() {
    let socket_path = env::args()
        .nth(1)
        .unwrap_or("/app/unix-socket-server.sock".to_string());
    let mut stream = UnixStream::connect(socket_path).await.unwrap();
    let buf = b"Stop copying me!";
    stream.write_all(buf).await.unwrap();
    let mut answer_buf = [0u8; 16];
    stream.read_exact(&mut answer_buf).await.unwrap();
    assert_eq!(&answer_buf, buf)
}
