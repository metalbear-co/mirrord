use std::collections::HashSet;

use const_format::concatcp;
use drain::Watch;
use iptables;
use mirrord_protocol::Port;
use tokio::{
    net::TcpListener,
    sync::mpsc::{Receiver, Sender},
};
use tracing::debug;

struct SafeIPTables {
    pub inner: iptables::IPTables,
}

const IPTABLES_TABLE_NAME: &str = "nat";
const MIRRORD_CHAIN_NAME: &str = "MIRRORD_REDIRECT";

enum StealInput {
    AddPort(Port),
}

enum StealOutput {}

impl SafeIPTables {
    pub fn new() -> Self {
        let ipt = iptables::new(false).unwrap();
        ipt.new_chain(IPTABLES_TABLE_NAME, MIRRORD_CHAIN_NAME)
            .unwrap();
        ipt.append(IPTABLES_TABLE_NAME, MIRRORD_CHAIN_NAME, "-j RETURN")
            .unwrap();
        ipt.append(
            IPTABLES_TABLE_NAME,
            "PREROUTING",
            concatcp!("-j ", MIRRORD_CHAIN_NAME),
        )
        .unwrap();
        Self { inner: ipt }
    }
}

impl Drop for SafeIPTables {
    fn drop(&mut self) {
        let _ = self
            .inner
            .delete(
                IPTABLES_TABLE_NAME,
                "PREROUTING",
                concatcp!("-j ", MIRRORD_CHAIN_NAME),
            )
            .unwrap();
        let _ = self
            .inner
            .delete_chain(IPTABLES_TABLE_NAME, MIRRORD_CHAIN_NAME);
    }
}

fn format_redirect_rule(redirected_port: Port, target_port: Port) -> String {
    format!(
        "-p tcp -m tcp --dport {} -j REDIRECT --to-ports {}",
        redirected_port, target_port
    )
}

async fn steal_worker(mut rx: Receiver<StealInput>, tx: Sender<StealOutput>, watch: Watch) {
    let ipt = SafeIPTables::new();
    let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
    let listen_port = listener.local_addr().unwrap().port();
    let mut ports: HashSet<Port> = HashSet::new();

    loop {
        select! {
            Some(msg) = rx.recv() => {
                match msg {
                Ok(StealInput::AddPort(port)) => {
                    if ports.insert(port) {
                        ipt.inner.append(IPTABLES_TABLE_NAME, MIRRORD_CHAIN_NAME, format_redirect_rule(port, listen_port)).unwrap();
                    } else {
                        debug!("Port already added {port:?}");
                    }
                }
            },
        }
        }
    }
}
