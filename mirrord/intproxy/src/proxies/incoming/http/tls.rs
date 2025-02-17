use std::sync::Arc;

use mirrord_tls_util::DangerousNoVerifierServer;
use rustls::ClientConfig;
use tokio_rustls::TlsConnector;

/// Creates a [`TlsConnector`] that accepts all server certificates and offers no self
/// authentication.
pub fn make_tls_connector(alpn_protocol: Option<Vec<u8>>) -> TlsConnector {
    let mut client_config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(DangerousNoVerifierServer))
        .with_no_client_auth();

    if let Some(alpn_protocol) = alpn_protocol {
        client_config.alpn_protocols.push(alpn_protocol);
    }

    TlsConnector::from(Arc::new(client_config))
}
