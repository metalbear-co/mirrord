use std::{
    net::{SocketAddr, TcpStream},
    os::fd::{AsRawFd, FromRawFd, RawFd},
    process::Command,
};

/// The remote peer the test answers the connect request for. The layer reports it from its socket
/// bookkeeping, while the socket itself is connected to the local intproxy.
const PEER: &str = "1.1.1.1:4567";

#[allow(
    clippy::disallowed_methods,
    reason = "exercises the layer's exec hooks through `Command`'s `posix_spawn` path"
)]
fn main() {
    let peer = PEER.parse::<SocketAddr>().unwrap();

    if let Some(fd) = std::env::args().nth(1) {
        // SAFETY: the parent passes a connected socket that it left open across `exec`.
        let stream = unsafe { TcpStream::from_raw_fd(fd.parse::<RawFd>().unwrap()) };
        // Only the socket metadata the parent handed over lets the layer in this image report the
        // remote peer. Without it, `getpeername` returns the intproxy's local address.
        assert_eq!(stream.peer_addr().unwrap(), peer);
        return;
    }

    let stream = TcpStream::connect(peer).unwrap();
    assert_eq!(stream.peer_addr().unwrap(), peer);

    // Rust opens sockets with `FD_CLOEXEC`, and the layer in a new image only keeps the sockets
    // whose descriptor survived `exec`.
    let fd = stream.as_raw_fd();
    assert_eq!(unsafe { libc::fcntl(fd, libc::F_SETFD, 0) }, 0);

    let executable = std::env::current_exe().unwrap();
    // `Command` without a pre-exec hook uses glibc's `posix_spawn`, which runs `execve` from a
    // `vfork` child on this thread. The second child only gets the metadata if the first `execve`
    // left no replacement state behind on this thread.
    for _ in 0..2 {
        assert!(
            Command::new(&executable)
                .arg(fd.to_string())
                .status()
                .unwrap()
                .success()
        );
    }
}
