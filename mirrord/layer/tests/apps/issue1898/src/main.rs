use std::{
    net::{Shutdown, SocketAddr, TcpStream as SyncTcpStream},
    time::Duration,
};

use async_std::{net::TcpStream as AsyncTcpStream, task::sleep};
use futures_lite::future;

fn main() {
    println!("test issue 1898: START");

    let socket_addr: SocketAddr = "1.2.3.4:80".parse().unwrap();
    let second_socket_addr: SocketAddr = "2.3.4.5:80".parse().unwrap();

    let sync_stream = async_global_executor::spawn_blocking(move || {
        let stream = SyncTcpStream::connect(socket_addr).expect("sync tcp stream was not created");

        stream
            .shutdown(Shutdown::Both)
            .expect("unable to shutdown sync tcp stream");
    });

    let async_stream = async_global_executor::spawn(async move {
        sleep(Duration::from_millis(300)).await;

        let stream = AsyncTcpStream::connect(second_socket_addr)
            .await
            .expect("sync tcp stream was not created");

        stream
            .shutdown(Shutdown::Both)
            .expect("unable to shutdown sync tcp stream");
    });

    async_global_executor::block_on(future::zip(async_stream, sync_stream));

    println!("test issue 1898: SUCCESS");
}
