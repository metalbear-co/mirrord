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

/// A [`reqwest::ClientBuilder`] that trusts the bundled roots alongside the platform's.
///
/// reqwest's own default fails to build a client on a Linux system without any CA certificates.
/// The bundled roots are merged in rather than handed over as a preconfigured rustls config, which
/// reqwest accepts only when it is the exact `rustls` version reqwest links, and otherwise rejects
/// at `build()` with an opaque builder error.
pub(crate) fn http_client_builder() -> Result<reqwest::ClientBuilder, SessionsManagerClientError> {
    let bundled = TLS_SERVER_ROOT_CERTS
        .iter()
        .map(|certificate| reqwest::Certificate::from_der(certificate))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(reqwest::Client::builder().tls_certs_merge(bundled))
}

/// The rustls equivalent of [`http_client_builder`], for the data-plane WebSocket, which reqwest
/// does not dial.
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
