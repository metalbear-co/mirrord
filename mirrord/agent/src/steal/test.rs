//! Tests for the whole [`steal`](super) module.
//!
//! Verify the whole flow from the [`TcpStealerApi`](super::api::TcpStealerApi)
//! to the [`RedirectorTask`](crate::incoming::RedirectorTask).

use std::{sync::Arc, time::Duration};

use hyper_util::rt::TokioIo;
use mirrord_protocol::{
    tcp::{DaemonTcp, Filter, HttpFilter, LayerTcpSteal, StealType},
    DaemonMessage,
};
use mirrord_tls_util::MaybeTls;
use rstest::rstest;
use rustls::pki_types::ServerName;
use tokio::{net::TcpListener, sync::mpsc, task::JoinSet};
use tokio_rustls::{TlsAcceptor, TlsConnector, TlsStream};
use utils::{HttpKind, TcpProtocolKind};

use super::{TcpStealerApi, TcpStealerTask};
use crate::{
    http::sender::HttpSender,
    incoming::{test::DummyRedirector, tls::test::SimpleStore, RedirectorTask},
    util::remote_runtime::{BgTaskRuntime, IntoStatus},
};

mod utils;

/// Verifies that redirected request upgrades are handled correctly.
///
/// The HTTP client sends one POST request with streaming body,
/// then a GET request with an upgrade.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn request_upgrade(
    #[values(false)] stolen: bool,
    #[values(HttpKind::Http1, HttpKind::Http1Alpn, HttpKind::Http1NoAlpn)] http_kind: HttpKind,
    #[values(
        TcpProtocolKind::Echo,
        TcpProtocolKind::ClientTalks,
        TcpProtocolKind::ServerTalks
    )]
    upgraded_protocol: TcpProtocolKind,
) {
    // Initial setup
    let original_server = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let original_destination = original_server.local_addr().unwrap();
    let tls_setup = if http_kind.uses_tls() {
        Some(SimpleStore::new(original_destination.port(), &["h2", "http/1.1"]).await)
    } else {
        None
    };
    let (redirector, _redirector_state, mut conn_tx) = DummyRedirector::new();
    let (redirector, handle) = RedirectorTask::new(
        redirector,
        tls_setup
            .as_ref()
            .map(|setup| setup.store.clone())
            .unwrap_or_default(),
    );
    let (command_tx, command_rx) = mpsc::channel(8);
    let stealer_task = TcpStealerTask::new(command_rx, handle);
    tokio::spawn(redirector.run());
    let stealer_status = BgTaskRuntime::Local
        .spawn(stealer_task.run())
        .into_status("stealer");

    // Stealing client makes filtered subscription.
    // This has to be done synchronously in this test,
    // because otherwise the HTTP connection might be dropped.
    let mut api = TcpStealerApi::new(0, "1.19.4".parse().unwrap(), command_tx, stealer_status)
        .await
        .unwrap();
    api.handle_client_message(LayerTcpSteal::PortSubscribe(StealType::FilteredHttpEx(
        original_destination.port(),
        HttpFilter::Path(Filter::new("/api/v1".into()).unwrap()),
    )))
    .await
    .unwrap();
    assert_eq!(
        api.recv().await.unwrap(),
        DaemonMessage::TcpSteal(DaemonTcp::SubscribeResult(Ok(original_destination.port()))),
    );

    // Run all tasks in parallel.
    let mut tasks = JoinSet::new();

    // HTTP client task.
    let http_client_conn = conn_tx.make_connection(original_destination).await;
    let http_client_tls = tls_setup.as_ref().map(|setup| setup.client_config.clone());
    tasks.spawn(async move {
        let conn = match http_client_tls {
            Some(mut config) => {
                if let Some(alpn) = http_kind.alpn() {
                    config.alpn_protocols.push(alpn.as_bytes().to_vec());
                }
                let connector = TlsConnector::from(Arc::new(config));
                let tls_conn = connector
                    .connect(ServerName::try_from("server").unwrap(), http_client_conn)
                    .await
                    .unwrap();
                MaybeTls::Tls(Box::new(TlsStream::Client(tls_conn)))
            }
            None => MaybeTls::NoTls(http_client_conn),
        };
        let mut sender = HttpSender::new(TokioIo::new(conn), http_kind.version())
            .await
            .unwrap();
        println!("[{}:{}] HTTP client connected", file!(), line!());

        let path = if stolen { "/api/v1" } else { "/api/v2" };
        utils::send_echo_request(&mut sender, http_kind.uses_tls(), path, None).await;
        utils::send_upgrade_request(
            &mut sender,
            http_kind.uses_tls(),
            path,
            None,
            upgraded_protocol,
        )
        .await;
    });

    // Original HTTP server task.
    let original_server_tls = tls_setup
        .as_ref()
        .map(|setup| setup.server_config.clone())
        .map(Arc::new)
        .map(TlsAcceptor::from);
    tasks.spawn(async move {
        let (stream, _) = original_server.accept().await.unwrap();
        utils::test_http_server(stream, http_kind.version(), original_server_tls.as_ref()).await;
        let (stream, _) = original_server.accept().await.unwrap();
        utils::test_http_server(stream, http_kind.version(), original_server_tls.as_ref()).await;
    });

    // Stealing client task.
    tasks.spawn(async move {});

    tasks.join_all().await;
}
