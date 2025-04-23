//! Tests for redirecting TCP connections with the
//! [`RedirectorTask`](crate::incoming::RedirectorTask).

use std::{
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};

use futures::StreamExt;
use rstest::rstest;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinSet,
};

use super::dummy_redirector::DummyRedirector;
use crate::incoming::{IncomingStreamItem, IncomingTlsHandlerStore, RedirectorTask};

async fn echo_tcp_server(listener: TcpListener) {
    let mut conn = listener.accept().await.unwrap().0;
    let mut buffer = [0_u8; 64];

    loop {
        let bytes = conn.read(&mut buffer).await.unwrap();
        if bytes == 0 {
            conn.shutdown().await.unwrap();
            break;
        }
        conn.write_all(&buffer[..bytes]).await.unwrap();
    }
}

async fn echo_tcp_client(mut stream: TcpStream, message: &[u8], times: usize) {
    let mut buffer = vec![0_u8; message.len()];

    for _ in 0..times {
        stream.write_all(message).await.unwrap();
        stream.read_exact(buffer.as_mut_slice()).await.unwrap();
        assert_eq!(&buffer, message);
    }

    stream.shutdown().await.unwrap();

    let bytes = stream.read(&mut buffer).await.unwrap();
    assert_eq!(bytes, 0);
}

/// Verifies full TCP flow on a single TCP connection and various subscription options.
#[rstest]
#[case::steal_and_mirror(true, true)]
#[case::steal(true, false)]
#[case::mirror(false, true)]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn tcp_full_flow(#[case] steal: bool, #[case] mirror: bool) {
    assert!(steal || mirror, "this test cannot handle no subscription");

    let listener = TcpListener::bind(SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 0))
        .await
        .unwrap();
    let destination = listener.local_addr().unwrap();

    let (redirector, _, connections) = DummyRedirector::new();
    let (task, mut steal_handle, mut mirror_handle) =
        RedirectorTask::new(redirector, IncomingTlsHandlerStore::dummy());
    tokio::spawn(task.run());

    if steal {
        steal_handle.steal(destination.port()).await.unwrap();
    }
    if mirror {
        mirror_handle.mirror(destination.port()).await.unwrap();
    }

    let mut tasks = JoinSet::new();
    if steal {
        tasks.spawn(async move {
            let mut conn = steal_handle
                .next()
                .await
                .unwrap()
                .unwrap()
                .unwrap_tcp()
                .steal();
            for _ in 0..5 {
                conn.data_tx.send(b" hello there".into()).await.unwrap();
                conn.stream.assert_data(b" hello there").await;
            }
            std::mem::drop(conn.data_tx);
            let item = conn.stream.next().await.unwrap();
            assert!(matches!(&item, IncomingStreamItem::NoMoreData), "{item:?}");
            let item = conn.stream.next().await.unwrap();
            assert!(
                matches!(&item, IncomingStreamItem::Finished(Ok(()))),
                "{item:?}"
            );
        });
    } else {
        tasks.spawn(echo_tcp_server(listener));
    }

    if mirror {
        tasks.spawn(async move {
            let mut conn = mirror_handle.next().await.unwrap().unwrap().unwrap_tcp();
            let expected = std::iter::repeat_n(b" hello there", 5)
                .flatten()
                .copied()
                .collect::<Vec<_>>();
            conn.stream.assert_data(&expected).await;
            let item = conn.stream.next().await.unwrap();
            assert!(matches!(&item, IncomingStreamItem::NoMoreData), "{item:?}");
            let item = conn.stream.next().await.unwrap();
            assert!(matches!(item, IncomingStreamItem::Finished(Ok(()))));
        });
    }

    let conn = connections.new_tcp(destination).await;
    echo_tcp_client(conn, b" hello there", 5).await;

    tasks.join_all().await;
}
