//! A local HTTPS server standing in for a Kubernetes API server with its own CA.

use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use http_body_util::Full;
use hyper::{
    Request, Response, StatusCode,
    body::{Bytes, Incoming},
    header::{
        AUTHORIZATION, CONNECTION, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_KEY, SEC_WEBSOCKET_PROTOCOL,
        UPGRADE,
    },
    server::conn::http1,
    service::service_fn,
};
use hyper_util::rt::TokioIo;
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer},
};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, handshake::derive_accept_key, protocol::Role},
};

/// `Authorization` header of every request the server received.
pub type SeenAuthorizations = Arc<Mutex<Vec<Option<String>>>>;

/// Serves TLS with a fresh self-signed certificate for `localhost`. Plain requests get `{}`;
/// WebSocket upgrades are accepted with the API server's subprotocol and sent one `hello`.
///
/// Returns the address and the certificate as PEM.
pub async fn start_server(seen: SeenAuthorizations) -> (SocketAddr, String) {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
    let certificate_pem = certified.cert.pem();
    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(certified.cert.der().to_vec())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                certified.signing_key.serialize_der(),
            )),
        )
        .unwrap();
    let acceptor = TlsAcceptor::from(Arc::new(server_config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let acceptor = acceptor.clone();
            let seen = seen.clone();
            tokio::spawn(async move {
                let Ok(stream) = acceptor.accept(stream).await else {
                    return;
                };
                let service = service_fn(move |request| handle(request, seen.clone()));
                let _ = http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .with_upgrades()
                    .await;
            });
        }
    });

    (address, certificate_pem)
}

async fn handle(
    mut request: Request<Incoming>,
    seen: SeenAuthorizations,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    seen.lock().unwrap().push(
        request
            .headers()
            .get(AUTHORIZATION)
            .map(|value| value.to_str().unwrap().to_owned()),
    );

    let Some(key) = request.headers().get(SEC_WEBSOCKET_KEY).cloned() else {
        return Ok(Response::new(Full::new(Bytes::from_static(b"{}"))));
    };

    let upgrade = hyper::upgrade::on(&mut request);
    tokio::spawn(async move {
        let upgraded = upgrade.await.unwrap();
        let mut socket =
            WebSocketStream::from_raw_socket(TokioIo::new(upgraded), Role::Server, None).await;
        futures::SinkExt::send(&mut socket, Message::text("hello"))
            .await
            .unwrap();
    });

    Ok(Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header(UPGRADE, "websocket")
        .header(CONNECTION, "Upgrade")
        .header(SEC_WEBSOCKET_ACCEPT, derive_accept_key(key.as_bytes()))
        .header(SEC_WEBSOCKET_PROTOCOL, "v4.channel.k8s.io")
        .body(Full::default())
        .unwrap())
}
