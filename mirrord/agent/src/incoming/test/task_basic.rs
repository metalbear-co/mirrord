//! Basic [`PortRedirector`](crate::incoming::PortRedirector) tests.

use std::{
    net::{Ipv4Addr, SocketAddr},
    time::Duration,
};

use rstest::rstest;
use tokio::io::{AsyncWriteExt, Interest};

use crate::incoming::{
    test::dummy_redirector::DummyRedirector, tls::IncomingTlsHandlerStore, MirroredTraffic,
    Redirected, RedirectorTask, StolenTraffic,
};

/// Verifies that the [`RedirectorTask`] cleans up its state when handle's traffic channels (port
/// subscriptions) are dropped.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn cleanup_on_dead_channel() {
    let (redirector, mut state, _tx) = DummyRedirector::new();
    let (task, mut steal_handle, mut mirror_handle) =
        RedirectorTask::new(redirector, IncomingTlsHandlerStore::dummy());
    tokio::spawn(task.run());

    steal_handle.steal(80).await.unwrap();
    assert!(state.borrow().has_redirections([80]));

    steal_handle.stop_steal(80);
    state
        .wait_for(|state| state.has_redirections([]))
        .await
        .unwrap();

    steal_handle.steal(81).await.unwrap();
    assert!(state.borrow().has_redirections([81]));

    mirror_handle.mirror(81).await.unwrap();
    mirror_handle.mirror(80).await.unwrap();
    assert!(state.borrow().has_redirections([80, 81]));

    std::mem::drop(steal_handle);

    mirror_handle.mirror(82).await.unwrap();
    assert!(state.borrow().has_redirections([80, 81, 82]));

    std::mem::drop(mirror_handle);

    state
        .wait_for(|state| state.has_redirections([]))
        .await
        .unwrap();
}

/// Verifies that the [`RedirectorTask`] correctly distributes incoming TCP traffic between the
/// subscribed clients.
#[rstest]
#[timeout(Duration::from_secs(5))]
#[tokio::test]
async fn concurrent_mirror_and_steal_tcp() {
    let destination = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 80);

    let (redirector, mut state, tx) = DummyRedirector::new();
    let (task, mut steal_handle, mut mirror_handle) =
        RedirectorTask::new(redirector, IncomingTlsHandlerStore::dummy());
    tokio::spawn(task.run());

    // Both handles should receive traffic.
    steal_handle.steal(destination.port()).await.unwrap();
    mirror_handle.mirror(destination.port()).await.unwrap();
    let mut redirected = Redirected::dummy(destination).await;
    let peer_addr = redirected.1.local_addr().unwrap();
    tx.send(redirected.0).await.unwrap();
    redirected
        .1
        .write_all(b"hello, this is not tcp")
        .await
        .unwrap();
    match steal_handle.next().await.unwrap().unwrap() {
        StolenTraffic::Tcp(redirected) => {
            assert_eq!(redirected.info().original_destination, destination);
            assert_eq!(redirected.info().peer_addr, peer_addr);
            assert!(redirected.info().tls_connector.is_none());
            redirected.steal()
        }
        StolenTraffic::Http(..) => panic!("Expected TCP traffic"),
    };
    match mirror_handle.next().await.unwrap().unwrap() {
        MirroredTraffic::Tcp(mirrored) => {
            assert_eq!(mirrored.info.original_destination, destination);
            assert!(mirrored.info.tls_connector.is_none());
            mirrored
        }
        MirroredTraffic::Http(..) => panic!("Expected TCP traffic"),
    };

    // New mirror handle should not inherit parent's subscriptions,
    // and should not receive traffic.
    let mut mirror_handle_2 = mirror_handle.clone();
    let mut redirected = Redirected::dummy(destination).await;
    let peer_addr = redirected.1.local_addr().unwrap();
    tx.send(redirected.0).await.unwrap();
    redirected
        .1
        .write_all(b"hello, this is not tcp")
        .await
        .unwrap();
    match steal_handle.next().await.unwrap().unwrap() {
        StolenTraffic::Tcp(redirected) => {
            assert_eq!(redirected.info().original_destination, destination);
            assert_eq!(redirected.info().peer_addr, peer_addr);
            assert!(redirected.info().tls_connector.is_none());
            redirected.steal()
        }
        StolenTraffic::Http(..) => panic!("Expected TCP traffic"),
    };
    match mirror_handle.next().await.unwrap().unwrap() {
        MirroredTraffic::Tcp(mirrored) => {
            assert_eq!(mirrored.info.original_destination, destination);
            assert!(mirrored.info.tls_connector.is_none());
            mirrored
        }
        MirroredTraffic::Http(..) => panic!("Expected TCP traffic"),
    };
    assert!(mirror_handle_2.next().await.is_none());

    // New mirror handle should receive traffic after making the subscription.
    mirror_handle_2.mirror(destination.port()).await.unwrap();
    let mut redirected = Redirected::dummy(destination).await;
    let peer_addr = redirected.1.local_addr().unwrap();
    tx.send(redirected.0).await.unwrap();
    redirected
        .1
        .write_all(b"hello, this is not tcp")
        .await
        .unwrap();
    match steal_handle.next().await.unwrap().unwrap() {
        StolenTraffic::Tcp(redirected) => {
            assert_eq!(redirected.info().original_destination, destination);
            assert_eq!(redirected.info().peer_addr, peer_addr);
            assert!(redirected.info().tls_connector.is_none());
            redirected.steal()
        }
        StolenTraffic::Http(..) => panic!("Expected TCP traffic"),
    };
    match mirror_handle.next().await.unwrap().unwrap() {
        MirroredTraffic::Tcp(mirrored) => {
            assert_eq!(mirrored.info.original_destination, destination);
            assert!(mirrored.info.tls_connector.is_none());
            mirrored
        }
        MirroredTraffic::Http(..) => panic!("Expected TCP traffic"),
    };
    match mirror_handle_2.next().await.unwrap().unwrap() {
        MirroredTraffic::Tcp(mirrored) => {
            assert_eq!(mirrored.info.original_destination, destination);
            assert!(mirrored.info.tls_connector.is_none());
            mirrored
        }
        MirroredTraffic::Http(..) => panic!("Expected TCP traffic"),
    };

    // Redirected connection should be dropped when all subscriptions are dropped.
    steal_handle.stop_steal(destination.port());
    mirror_handle.stop_mirror(destination.port());
    mirror_handle_2.stop_mirror(destination.port());
    state
        .wait_for(|state| state.has_redirections([]))
        .await
        .unwrap();
    let redirected = Redirected::dummy(destination).await;
    tx.send(redirected.0).await.unwrap();
    let ready = redirected.1.ready(Interest::READABLE).await.unwrap();
    assert!(ready.is_read_closed());
}
