use std::{fs::File, net::TcpListener, os::fd::AsRawFd};

fn main() {
    let listener = TcpListener::bind("127.0.0.1:4567").expect("tcp listener bind");

    let raw_fd = listener.as_raw_fd();
    if unsafe { libc::listen(raw_fd, 1024) } != 0 {
        panic!("second listen failed");
    }

    // trigger a trivial proxy message
    File::open("/why_double_listen").expect("file open failed");
}
