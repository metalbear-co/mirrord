use std::{
    io::Write,
    net::{Shutdown, SocketAddr, TcpStream as SyncTcpStream},
};

use tokio::{io::AsyncWriteExt, net::TcpStream as AsyncTcpStream, task};

#[tokio::main]
async fn main() {
    println!("test issue 1898: START");

    let socket_addr_1: SocketAddr = "1.2.3.4:80".parse().unwrap();
    let socket_addr_2: SocketAddr = "2.3.4.5:80".parse().unwrap();

    let handle_1 = task::spawn_blocking(move || {
        let mut stream = SyncTcpStream::connect(socket_addr_1).expect("sync tcp connect failed");
        stream
            .write_all(b"hello")
            .expect("failed to write data via sync tcp stream");
        stream
            .shutdown(Shutdown::Both)
            .expect("failed to shutdown sync tcp stream");
    });

    let handle_2 = task::spawn(async move {
        let mut stream = AsyncTcpStream::connect(socket_addr_2)
            .await
            .expect("async tcp connect failed");
        stream
            .write_all(b"hello")
            .await
            .expect("failed to write data via async tcp stream");
        stream
            .shutdown()
            .await
            .expect("failed to shutdown async tcp stream");
    });

    tokio::try_join!(handle_1, handle_2).expect("one of the connect methods failed");

    println!("test issue 1898: SUCCESS");
}
