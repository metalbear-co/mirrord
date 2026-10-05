//! [`ClusterClientFactory::build_client`] against a local HTTPS server standing in for a
//! Kubernetes API server with its own CA.

mod common;

use std::{net::SocketAddr, time::Duration};

use base64::{Engine, engine::general_purpose::STANDARD};
use common::{SeenAuthorizations, start_server};
use futures::StreamExt;
use hyper::Request;
use mirrord_cluster_auth::{AuthMethod, ClusterClientFactory, ClusterCredentials};
use tokio_tungstenite::tungstenite::Message;

fn credentials(address: SocketAddr, certificate_pem: &str, token: &str) -> ClusterCredentials {
    ClusterCredentials {
        name: format!("test-{}", address.port()),
        server: format!("https://localhost:{}", address.port()),
        namespace: "default".to_owned(),
        ca_data: Some(STANDARD.encode(certificate_pem)),
        auth_method: AuthMethod::BearerToken,
        token: Some(token.to_owned()),
        client_cert_pem: None,
        client_key_pem: None,
    }
}

#[tokio::test]
async fn client_trusts_the_cluster_ca_and_sends_the_token() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let seen = SeenAuthorizations::default();
    let (address, certificate_pem) = start_server(seen.clone()).await;

    let factory = ClusterClientFactory::new().unwrap();
    let token_directory = factory.token_files_dir().to_owned();
    let client = factory
        .build_client(&credentials(address, &certificate_pem, "token-1"))
        .await
        .unwrap();
    let clone = client.clone();
    drop(factory);
    assert!(token_directory.exists());
    client
        .request_text(Request::get("/version").body(Vec::new()).unwrap())
        .await
        .unwrap();

    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [Some("Bearer token-1".to_owned())]
    );
    drop(client);
    assert!(token_directory.exists());
    drop(clone);
    tokio::time::timeout(Duration::from_secs(2), async {
        while token_directory.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn client_upgrades_to_a_websocket() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let seen = SeenAuthorizations::default();
    let (address, certificate_pem) = start_server(seen.clone()).await;

    let client = ClusterClientFactory::new()
        .unwrap()
        .build_client(&credentials(address, &certificate_pem, "token-1"))
        .await
        .unwrap();
    let mut socket = mirrord_operator_websocket::upgrade::connect_ws(
        &client,
        Request::get("/ws").body(Vec::new()).unwrap(),
    )
    .await
    .unwrap();

    let message = socket.next().await.unwrap().unwrap();
    assert_eq!(message, Message::text("hello"));
    assert_eq!(
        seen.lock().unwrap().as_slice(),
        [Some("Bearer token-1".to_owned())]
    );
}

#[tokio::test]
async fn client_rejects_a_server_outside_the_cluster_ca() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let (address, _) = start_server(SeenAuthorizations::default()).await;
    let other_ca = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
        .unwrap()
        .cert
        .pem();

    let client = ClusterClientFactory::new()
        .unwrap()
        .build_client(&credentials(address, &other_ca, "token-1"))
        .await
        .unwrap();

    client
        .request_text(Request::get("/version").body(Vec::new()).unwrap())
        .await
        .unwrap_err();
}
