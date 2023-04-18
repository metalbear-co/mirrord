use std::{
    env,
    io::{Read, Write},
    net::{SocketAddr, TcpStream, UdpSocket},
};

const MESSAGE: &[u8] = "FOO BAR HAM".as_bytes();

fn parse_args() -> Option<(bool, SocketAddr, Vec<SocketAddr>)> {
    let args = env::args().collect::<Vec<_>>();

    if args.len() != 4 {
        None?
    }

    let tcp = match args[1].as_str() {
        "--tcp" => true,
        "--udp" => false,
        _ => None?,
    };
    let socket = args[2].parse::<SocketAddr>().ok()?;
    let peers = args[3]
        .split(',')
        .map(|s| s.parse::<SocketAddr>())
        .collect::<Result<Vec<_>, _>>()
        .ok()?;

    Some((tcp, socket, peers))
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
        panic!("Invalid response received: {:?}.", &response[..res_len]);
    }
    if remote != peer {
        panic!("Invalid peer address from recv: {remote}.");
    }
}

fn main() {
    let Some((use_tcp, socket, peers)) = parse_args() else {
        panic!("USAGE: {} --tcp/--udp <local socket> <peer sockets>", env::args().next().unwrap());
    };

    peers.into_iter().for_each(|peer| {
        if use_tcp {
            test_tcp(socket, peer);
        } else {
            test_udp(socket, peer);
        }
    });
}
