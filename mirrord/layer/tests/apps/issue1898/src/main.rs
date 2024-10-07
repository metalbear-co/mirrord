use std::{
    net::{Shutdown, SocketAddr, TcpStream as SyncTcpStream},
    time::Duration,
};

use async_std::net::TcpStream as AsyncTcpStream;
use thread_async::ThreadFutureExt;

fn main() {
    println!("test issue 1898: START");

    let socket_addr: SocketAddr = "1.2.3.4:80".parse().unwrap();
    let second_socket_addr: SocketAddr = "2.3.4.5:80".parse().unwrap();

    let async_stream = AsyncTcpStream::connect(socket_addr).spawn_thread_await();

    assert!(!async_stream.is_finished());

    std::thread::sleep(Duration::from_millis(10));

    let stream =
        SyncTcpStream::connect(second_socket_addr).expect("sync tcp stream was not created");

    stream
        .shutdown(Shutdown::Both)
        .expect("unable to shutdown sync tcp stream");

    async_stream
        .join()
        .expect("unable to join async connect")
        .expect("sync tcp stream was not created")
        .shutdown(Shutdown::Both)
        .expect("unable to shutdown sync tcp stream");

    println!("test issue 1898: SUCCESS");
}
