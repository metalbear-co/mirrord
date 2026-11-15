//! Which certificate authorities the sessions-manager and MetalBear Cloud connections trust.
//!
//! The workload companion runs inside the customer's application container, and many images ship
//! no CA bundle at all (slim and distroless bases, or any image without `ca-certificates`). The
//! Mozilla roots compiled into the binary are therefore always trusted, so reaching MetalBear
//! Cloud, or any sessions-manager with a publicly trusted certificate, needs nothing from the image.
//!
//! The platform's own roots are trusted as well whenever it has any, so a sessions-manager behind a
//! private CA or a TLS-inspecting proxy keeps working once that CA is installed in the image (or on
//! the developer's machine, for the CLI). The control plane and the data plane share this trust, so
//! a deployment cannot pass registration and then fail every data-plane dial.

use std::sync::Arc;

use rustls::{ClientConfig, crypto::aws_lc_rs};
use rustls_platform_verifier::Verifier;
use webpki_root_certs::TLS_SERVER_ROOT_CERTS;

use crate::error::SessionsManagerClientError;

/// A [`ClientConfig`] that trusts the bundled roots alongside the platform's.
///
/// reqwest's and rustls' own defaults fail on a Linux system without any CA certificates.
pub(crate) fn rustls_config() -> Result<ClientConfig, SessionsManagerClientError> {
    let provider = Arc::new(aws_lc_rs::default_provider());
    let verifier =
        Verifier::new_with_extra_roots(TLS_SERVER_ROOT_CERTS.iter().cloned(), provider.clone())?;

    Ok(ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth())
}

/// A [`reqwest::ClientBuilder`] that verifies servers with `config`, from [`rustls_config`].
///
/// reqwest negotiates HTTP/2 only when the preconfigured `config` advertises it over ALPN.
pub(crate) fn http_client_builder(mut config: ClientConfig) -> reqwest::ClientBuilder {
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    reqwest::Client::builder().tls_backend_preconfigured(config)
}
