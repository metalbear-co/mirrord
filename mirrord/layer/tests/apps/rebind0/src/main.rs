//! Application that binds port 0 (any port) twice, and panics if it fails.

use std::net::UdpSocket;

fn main() {
    let _soc = UdpSocket::bind("0.0.0.0:0").expect("Could not even bind once, this is unexpected.");
    let _soc =
        UdpSocket::bind("0.0.0.0:0").expect("binding port 0 again failed, but it should succeed.");
}
