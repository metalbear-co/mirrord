#![cfg(test)]

use std::{
    net::{Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Empty};
use hyper::Request;
use mirrord_agent_env::steal_tls::{
    AgentClientConfig, AgentServerConfig, IncomingPortTlsConfig, TlsAuthentication,
    TlsServerVerification,
};
use mirrord_protocol::{
    tcp::{DaemonTcp, NewTcpConnectionV2, StealType, TrafficTransportType},
    DaemonMessage, LogLevel, LogMessage,
};
use pem::{EncodeConfig, LineEnding, Pem};
use rstest::rstest;
use rustls::{ClientConfig, RootCertStore};
use tokio::{
    io::AsyncWriteExt,
    sync::{mpsc, Notify},
};
use tokio_rustls::TlsConnector;

use crate::{
    http::HttpVersion,
    incoming::{test::start_redirector_task, tls::IncomingTlsHandlerStore},
    steal::TcpStealerTask,
    test::get_steal_api_with_subscription,
    util::{
        path_resolver::InTargetPathResolver,
        remote_runtime::{BgTaskRuntime, IntoStatus},
    },
};

/// Verifies that the client receives an error log when a TLS connection is not stolen due to
/// [`mirrord-protocol`] version.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn compat_test_tls_not_supported() {
    let destination = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 80);

    let tempdir = tempfile::tempdir().unwrap();
    let root_cert = mirrord_tls_util::generate_cert("root", None, true).unwrap();
    let server_cert = mirrord_tls_util::generate_cert("server", Some(&root_cert), false).unwrap();
    let cert_file = tempdir.path().join("cert.pem");
    let cert_pem = pem::encode_many_config(
        &[
            Pem::new("PRIVATE KEY", server_cert.key_pair.serialize_der()),
            Pem::new("CERTIFICATE", server_cert.cert.der().to_vec()),
            Pem::new("CERTIFICATE", root_cert.cert.der().to_vec()),
        ],
        EncodeConfig::new().set_line_ending(LineEnding::LF),
    );
    tokio::fs::write(cert_file, cert_pem).await.unwrap();
    let tls_config = IncomingPortTlsConfig {
        port: destination.port(),
        agent_as_server: AgentServerConfig {
            authentication: TlsAuthentication {
                cert_pem: "cert.pem".into(),
                key_pem: "cert.pem".into(),
            },
            verification: None,
            alpn_protocols: vec![],
        },
        agent_as_client: AgentClientConfig {
            authentication: None,
            verification: TlsServerVerification {
                accept_any_cert: false,
                trust_roots: vec!["cert.pem".into()],
            },
        },
    };
    let tls_store = IncomingTlsHandlerStore::new(
        vec![tls_config],
        InTargetPathResolver::with_root_path(tempdir.path().to_path_buf()),
    );

    let mut root_store = RootCertStore::empty();
    root_store.add(root_cert.cert.into()).unwrap();
    let connector = TlsConnector::from(Arc::new(
        ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth(),
    ));

    let (mut connections, steal_handle, _) = start_redirector_task(tls_store).await;
    let (stealer_tx, stealer_rx) = mpsc::channel(8);
    let stealer_task = TcpStealerTask::new(steal_handle, stealer_rx);
    let stealer_status = BgTaskRuntime::Local
        .spawn(stealer_task.run())
        .into_status("stealer task");
    let (mut steal, protocol) = get_steal_api_with_subscription(
        1,
        stealer_tx,
        stealer_status,
        Some("1.19.3"),
        StealType::All(destination.port()),
    )
    .await;

    let notify = Arc::new(Notify::new());
    let notify_cloned = notify.clone();

    tokio::spawn(async move {
        let connection = connections.new_tcp(destination).await;
        let mut connection = connector
            .connect("server".try_into().unwrap(), connection)
            .await
            .unwrap();
        connection
            .write_all(b" i am not http trust me")
            .await
            .unwrap();
        notify_cloned.notified().await;
        let connection = connections.new_tcp(destination).await;
        let mut connection = connector
            .connect("server".try_into().unwrap(), connection)
            .await
            .unwrap();
        connection
            .write_all(b" i am not http trust me")
            .await
            .unwrap();
    });

    match steal.recv().await.unwrap() {
        DaemonMessage::LogMessage(LogMessage {
            level: LogLevel::Error,
            ..
        }) => {}
        other => panic!("unexpected message: {other:?}"),
    }

    protocol.replace("1.19.4".parse().unwrap());
    notify.notify_one();

    match steal.recv().await.unwrap() {
        DaemonMessage::TcpSteal(DaemonTcp::NewConnectionV2(NewTcpConnectionV2 {
            transport: TrafficTransportType::Tls { .. },
            ..
        })) => {}
        other => panic!("unexpected message: {other:?}"),
    }
}

/// Verifies that the client receives an error log when an HTTP request is not stolen by an
/// unfiltered port subscription due to [`mirrord-protocol`] version.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn compat_test_http_in_unfiltered_not_supported() {
    let destination = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 80);

    let (mut connections, steal_handle, _) = start_redirector_task(Default::default()).await;
    let (stealer_tx, stealer_rx) = mpsc::channel(8);
    let stealer_task = TcpStealerTask::new(steal_handle, stealer_rx);
    let stealer_status = BgTaskRuntime::Local
        .spawn(stealer_task.run())
        .into_status("stealer task");
    let (mut steal, protocol) = get_steal_api_with_subscription(
        1,
        stealer_tx,
        stealer_status,
        Some("1.19.3"),
        StealType::All(destination.port()),
    )
    .await;

    let notify = Arc::new(Notify::new());
    let notify_cloned = notify.clone();

    tokio::spawn(async move {
        let mut request_sender = connections.new_http(destination, HttpVersion::V1).await;
        let _ = request_sender
            .send(Request::new(BoxBody::new(
                Empty::<Bytes>::new().map_err(|_| unreachable!()),
            )))
            .await
            .unwrap();
        notify_cloned.notified().await;
        let _ = request_sender
            .send(Request::new(BoxBody::new(
                Empty::<Bytes>::new().map_err(|_| unreachable!()),
            )))
            .await
            .unwrap();
    });

    match steal.recv().await.unwrap() {
        DaemonMessage::LogMessage(LogMessage {
            level: LogLevel::Error,
            ..
        }) => {}
        other => panic!("unexpected message: {other:?}"),
    }

    protocol.replace("1.19.4".parse().unwrap());
    notify.notify_one();

    match steal.recv().await.unwrap() {
        DaemonMessage::TcpSteal(DaemonTcp::HttpRequestChunked(..)) => {}
        other => panic!("unexpected message: {other:?}"),
    }
}
