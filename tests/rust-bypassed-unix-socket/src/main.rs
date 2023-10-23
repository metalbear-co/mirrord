#![warn(clippy::indexing_slicing)]

use std::env;

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{UnixListener, UnixStream},
};

const DEFAULT_SOCKET_ADDRESS: &str = "bypassed-unix-socket.sock";

async fn server(listener: UnixListener) {
    let (mut stream, addr) = listener.accept().await.unwrap();
    println!("Incoming connection from {addr:?}");
    let (mut reader, mut writer) = stream.split();
    let n = tokio::io::copy(&mut reader, &mut writer).await.unwrap();
    println!("Server echoed {n} bytes.");
}

async fn client(socket_path: String) {
    // Connect to that same socket.
    let mut stream = UnixStream::connect(socket_path).await.unwrap();

    let buf = b"Stop copying me!";
    stream.write_all(buf).await.unwrap();
    let mut answer_buf = [0u8; 16];
    stream.read_exact(&mut answer_buf).await.unwrap();
    assert_eq!(&answer_buf, buf)
}

#[tokio::main]
async fn main() {
    let socket_path = env::args()
        .nth(1)
        .unwrap_or_else(|| DEFAULT_SOCKET_ADDRESS.to_string());

    let _ = std::fs::remove_file(&socket_path);
    let listener = UnixListener::bind(&socket_path).unwrap();

    let server_handle = tokio::spawn(server(listener));
    let client_handle = tokio::spawn(client(socket_path.clone()));
    server_handle.await.unwrap();
    client_handle.await.unwrap();

    // Delete the socket file done, so it can be bound again in the next run.
    std::fs::remove_file(&socket_path).unwrap();
}
