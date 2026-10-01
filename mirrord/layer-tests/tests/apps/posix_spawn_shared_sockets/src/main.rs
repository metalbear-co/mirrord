use std::{
    net::{Ipv4Addr, SocketAddr, TcpListener},
    process::Command,
};

const SHARED_SOCKETS_ENV_VAR: &str = "MIRRORD_SHARED_SOCKETS";

fn main() {
    let mode = std::env::args().nth(1).expect("missing mode");

    if mode == "child" {
        assert!(std::env::var_os(SHARED_SOCKETS_ENV_VAR).is_some());
        return;
    }

    let _listener = TcpListener::bind(SocketAddr::new(Ipv4Addr::UNSPECIFIED.into(), 80)).unwrap();
    let executable = std::env::current_exe().unwrap();

    // `Command` without a pre-exec hook uses glibc's `posix_spawn` implementation.
    let spawn_child = || Command::new(&executable).arg("child").spawn().unwrap();
    let children = match mode.as_str() {
        "sequential" => {
            let mut child = spawn_child();
            assert!(child.wait().unwrap().success());
            vec![spawn_child()]
        }
        "concurrent" => vec![spawn_child(), spawn_child()],
        _ => panic!("unknown mode {mode}"),
    };

    for mut child in children {
        assert!(child.wait().unwrap().success());
    }
}
