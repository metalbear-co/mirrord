#![feature(assert_matches)]
#![warn(clippy::indexing_slicing)]

use std::{net::SocketAddr, path::PathBuf, time::Duration};

use futures::TryStreamExt;
use mirrord_protocol::{
    outgoing::{
        udp::LayerUdpOutgoing, DaemonConnect, DaemonRead, LayerConnect, LayerWrite, SocketAddress,
    },
    ClientMessage, DaemonMessage, FileRequest,
};
use rstest::rstest;

mod common;

pub use common::*;

#[rstest]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[timeout(Duration::from_secs(60))]
async fn test_dns_resolve(
    #[values(Application::RustDnsResolve)] application: Application,
    dylib_path: &PathBuf,
) {
    let (mut test_process, mut layer_connection) = application
        .start_process_with_layer(dylib_path, vec![], None)
        .await;

    let mut conn = layer_connection.codec;
    let msg = conn.try_next().await.unwrap().unwrap();

    println!("Message received from layer: {msg:?}");
}
