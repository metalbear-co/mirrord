#![warn(clippy::indexing_slicing)]

use std::{
    env,
    io::{Read, Write},
    net::{SocketAddr, TcpStream, UdpSocket},
    ops::Not,
};

use futures::{StreamExt, stream::FuturesUnordered};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const MESSAGE: &[u8] = "FOO BAR HAM".as_bytes();

fn parse_args() -> Option<(bool, SocketAddr, Vec<SocketAddr>, bool)> {
    let args = env::args().collect::<Vec<_>>();

    let tcp = match args.get(1)?.as_str() {
        "--tcp" => true,
        "--udp" => false,
        _ => None?,
    };
    let socket = args.get(2)?.parse::<SocketAddr>().ok()?;
    let peers = args
        .get(3)?
        .split(',')
        .map(|s| s.parse::<SocketAddr>())
        .collect::<Result<Vec<_>, _>>()
        .ok()?;

    let non_blocking = match args.get(4) {
        Some(arg) if arg == "--non-blocking" => true,
        Some(..) => None?,
        None => false,
    };

    Some((tcp, socket, peers, non_blocking))
}

fn test_tcp(socket: SocketAddr, peer: SocketAddr) {
    let mut conn = TcpStream::connect(peer).unwrap();

    let local = conn.local_addr().unwrap();
    if local != socket {
        panic!("Invalid local address from local_addr: {local}.")
    }

    let remote = conn.peer_addr().unwrap();
    if remote != peer {
        panic!("Invalid peer address from peer_addr: {peer}.");
    }

    conn.write_all(MESSAGE).unwrap();
    conn.flush().unwrap();

    let mut response = vec![];
    conn.read_to_end(&mut response).unwrap();

    if response != MESSAGE {
        panic!("Invalid response received: {response:?}");
    }
}

async fn test_tcp_non_blocking(socket: SocketAddr, peer: SocketAddr) {
    let mut conn = tokio::net::TcpStream::connect(peer).await.unwrap();

    let local = conn.local_addr().unwrap();
    if local != socket {
        panic!("Invalid local address from local_addr: {local}.")
    }

    let remote = conn.peer_addr().unwrap();
    if remote != peer {
        panic!("Invalid peer address from peer_addr: {peer}.");
    }

    conn.write_all(MESSAGE).await.unwrap();
    conn.flush().await.unwrap();

    let mut response = vec![];
    conn.read_to_end(&mut response).await.unwrap();

    if response != MESSAGE {
        panic!("Invalid response received: {response:?}");
    }
}

fn test_udp(socket: SocketAddr, peer: SocketAddr) {
    let udp_socket = UdpSocket::bind(socket).unwrap();
    udp_socket.connect(peer).unwrap();

    let local = udp_socket.local_addr().unwrap();
    if local != socket {
        panic!("Invalid local address from local_addr: {local}.")
    }

    let remote = udp_socket.peer_addr().unwrap();
    if remote != peer {
        panic!("Invalid peer address from peer_addr: {peer}.");
    }

    let sent = udp_socket.send(MESSAGE).unwrap();
    if sent != MESSAGE.len() {
        panic!("Partial send: {sent} bytes.");
    }

    let mut response = [0; MESSAGE.len()];
    let (res_len, remote) = udp_socket.recv_from(&mut response).unwrap();
    if res_len != MESSAGE.len() || response != MESSAGE {
        panic!(
            "Invalid response received: {:?}.",
            response
                .get(..res_len)
                .expect("returned response length out of bounds")
        );
    }
    if remote != peer {
        panic!("Invalid peer address from recv: {remote}.");
    }
}

fn main() {
    let Some((use_tcp, socket, peers, non_blocking)) = parse_args() else {
        panic!(
            "USAGE: {} --tcp/--udp <local socket> <peer sockets> [--non-blocking]",
            env::args().next().unwrap()
        );
    };

    if use_tcp.not() && non_blocking {
        panic!("--non-blocking is only supported with --tcp");
    }

    if non_blocking {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async move {
            let futures = FuturesUnordered::new();
            for peer in peers {
                futures.push(test_tcp_non_blocking(socket, peer));
            }
            futures.collect::<Vec<_>>().await;
        });
    } else {
        peers.into_iter().for_each(|peer| {
            if use_tcp {
                test_tcp(socket, peer);
            } else {
                test_udp(socket, peer);
            }
        });
    }
}
