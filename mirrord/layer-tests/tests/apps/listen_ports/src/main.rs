//! Test application that listens on a sequence of localhost ports.
//! On each port, it accepts one connection and expects to receive the string "HELLO" and a
//! shutdown.
//!
//! Reads the semicolon-separated port list from the `APP_PORTS` environment variable.
//! Reports bind outcomes on stdout so tests can wait for readiness and retry address collisions
//! without parsing panic messages.
//! Each announcement ends with a newline: `LISTENING PORT <port>` after a successful bind,
//! or `<ErrorKind:?> PORT <port>` before a failed bind panics.

use std::{
    io::Read,
    net::{Ipv4Addr, SocketAddr, TcpListener},
};

const PORTS_ENV: &str = "APP_PORTS";

fn main() {
    let ports = std::env::var(PORTS_ENV)
        .unwrap()
        .split(';')
        .map(|port| port.parse::<u16>().unwrap())
        .collect::<Vec<_>>();

    let mut buf = String::new();

    for port in ports {
        let addr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), port);
        let listener = TcpListener::bind(addr)
            .inspect_err(|error| println!("{:?} PORT {port}", error.kind()))
            .unwrap();
        println!("LISTENING PORT {port}");

        let (mut conn, _) = listener.accept().unwrap();
        conn.read_to_string(&mut buf).unwrap();
        assert_eq!(buf, "HELLO");

        buf.clear();
    }
}
