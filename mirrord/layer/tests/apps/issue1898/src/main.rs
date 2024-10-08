use std::{
    net::{Shutdown, SocketAddr, TcpStream as SyncTcpStream},
    time::Duration,
};

use async_std::net::TcpStream as AsyncTcpStream;
use futures_lite::future;

fn main() {
    println!("test issue 1898: START");

    let socket_addr: SocketAddr = "1.2.3.4:80".parse().unwrap();
    let second_socket_addr: SocketAddr = "2.3.4.5:80".parse().unwrap();

    let async_stream = async_global_executor::spawn(async move {
        let stream = AsyncTcpStream::connect(socket_addr)
            .await
            .expect("sync tcp stream was not created");

        stream
            .shutdown(Shutdown::Both)
            .expect("unable to shutdown sync tcp stream");
    });

    let sync_stream = async_global_executor::spawn_blocking(move || {
        std::thread::sleep(Duration::from_millis(100));

        let stream =
            SyncTcpStream::connect(second_socket_addr).expect("sync tcp stream was not created");

        stream
            .shutdown(Shutdown::Both)
            .expect("unable to shutdown sync tcp stream");
    });

    async_global_executor::block_on(future::zip(async_stream, sync_stream));

    println!("test issue 1898: SUCCESS");
}
