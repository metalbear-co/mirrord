#![warn(clippy::indexing_slicing)]

use std::{
    env,
    io::{Read, Write},
    net::{SocketAddr, TcpStream, UdpSocket},
};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    task::JoinSet,
};

const MESSAGE: &[u8] = "FOO BAR HAM".as_bytes();

struct Args {
    tcp: bool,
    expected_local_addr: SocketAddr,
    peers: Vec<SocketAddr>,
    non_blocking: bool,
}

fn parse_args() -> Option<Args> {
    let args = env::args().collect::<Vec<_>>();

    let tcp = match args.get(1)?.as_str() {
        "--tcp" => true,
        "--udp" => false,
        _ => None?,
    };
    let expected_local_addr = args.get(2)?.parse::<SocketAddr>().ok()?;
    let peers = args
        .get(3)?
        .split(',')
        .map(|s| s.parse::<SocketAddr>())
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let non_blocking = match args.get(3).map(String::as_str) {
        Some("--non-blocking") => true,
        None => false,
        Some(..) => None?,
    };

    Some(Args {
        tcp,
        expected_local_addr,
        peers,
        non_blocking,
    })
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

async fn test_tcp_non_blocking(socket: SocketAddr, peers: Vec<SocketAddr>) {
    let mut tasks = JoinSet::new();

    for peer in peers {
        tasks.spawn(async move {
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
        });
    }

    tasks.join_all().await;
}

fn main() {
    let Some(args) = parse_args() else {
        panic!(
            "USAGE: {} --tcp/--udp <local socket> <peer sockets> [--non-blocking]",
            env::args().next().unwrap()
        );
    };

    match (args.tcp, args.non_blocking) {
        (true, true) => {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(test_tcp_non_blocking(args.expected_local_addr, args.peers));
        }
        (true, false) => {
            args.peers
                .into_iter()
                .for_each(|peer| test_tcp(args.expected_local_addr, peer));
        }
        (false, true) => {
            panic!("--non-blocking flag is not supported with --udp")
        }
        (false, false) => {
            args.peers
                .into_iter()
                .for_each(|peer| test_udp(args.expected_local_addr, peer));
        }
    }
}
