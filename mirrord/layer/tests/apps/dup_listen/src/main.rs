use std::{
    io::Read,
    net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener},
    os::fd::AsRawFd,
};

fn main() {
    let listener = TcpListener::bind(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 80)).unwrap();

    // Duplicate the listener's descriptor and close it.
    let fd = listener.as_raw_fd();
    let fd_2 = nix::unistd::dup(fd).unwrap();
    nix::unistd::close(fd_2).unwrap();
    // Test code waits for this message.
    println!("Duplicated descriptor closed");

    // Listener should still be able to get remote traffic.
    let (mut stream, peer) = listener.accept().unwrap();
    println!("Accepted incoming connection from {peer}");

    stream.shutdown(Shutdown::Write).unwrap();
    let mut buf = String::new();
    stream.read_to_string(&mut buf).unwrap();
    assert_eq!(buf, "hello there");
}
