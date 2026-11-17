//! Which certificate authorities the sessions-manager and MetalBear Cloud connections trust.
//!
//! The workload companion runs inside the customer's application container, and some images ship
//! no CA bundle at all (`debian:*-slim`, or any other image without `ca-certificates`). The
//! Mozilla roots compiled into the binary are therefore always trusted (all but the few Mozilla
//! trusts only for some names, see [`bundled_roots`]), so reaching MetalBear Cloud, or any
//! sessions-manager with a publicly trusted certificate, needs nothing from the image.
//!
//! The platform's own roots are trusted as well whenever it has any, so a sessions-manager behind a
//! private CA or a TLS-inspecting proxy keeps working once that CA is installed in the image (or on
//! the developer's machine, for the CLI). The control plane and the data plane share this trust, so
//! a certificate one of them accepts the other accepts too. They don't share a route: reqwest
//! honors `HTTPS_PROXY` and `ALL_PROXY`, while the data-plane WebSocket always dials directly.

use std::sync::Arc;

use rustls::{ClientConfig, crypto::aws_lc_rs, pki_types::CertificateDer};
use rustls_platform_verifier::Verifier;
use webpki_root_certs::TLS_SERVER_ROOT_CERTS;
use webpki_roots::TLS_SERVER_ROOTS;

use crate::error::SessionsManagerClientError;

/// The bundled Mozilla roots, without those Mozilla trusts only for some names.
///
/// Both reqwest and the platform verifier take roots as bare certificates, which can't carry the
/// name constraints Mozilla attaches to a root (such as TUBITAK Kamu SM's, limited to `.tr`).
/// Trusted unconstrained, such a root could issue a certificate for any sessions-manager, so it's
/// left out. `webpki-roots` lists the same roots with their constraints, so a certificate is kept
/// only when `webpki-roots` lists its subject and key without constraints. The two crates are
/// released separately, and a root only the newer one knows is left out rather than trusted for
/// every name.
fn bundled_roots() -> impl Iterator<Item = CertificateDer<'static>> {
    TLS_SERVER_ROOT_CERTS
        .iter()
        .filter(|certificate| {
            webpki::anchor_from_trusted_cert(certificate).is_ok_and(|anchor| {
                TLS_SERVER_ROOTS.iter().any(|listed| {
                    listed.name_constraints.is_none()
                        && listed.subject == anchor.subject
                        && listed.subject_public_key_info == anchor.subject_public_key_info
                })
            })
        })
        .cloned()
}

/// A [`reqwest::ClientBuilder`] that trusts the bundled roots alongside the platform's.
///
/// reqwest's own default fails to build a client on a Linux system without any CA certificates.
/// The bundled roots are merged in rather than handed over as a preconfigured rustls config, which
/// reqwest accepts only when it is the exact `rustls` version reqwest links, and otherwise rejects
/// at `build()` with an opaque builder error.
pub(crate) fn http_client_builder() -> Result<reqwest::ClientBuilder, SessionsManagerClientError> {
    let bundled = bundled_roots()
        .map(|certificate| reqwest::Certificate::from_der(&certificate))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(reqwest::Client::builder().tls_certs_merge(bundled))
}

/// The rustls equivalent of [`http_client_builder`], for the data-plane WebSocket, which reqwest
/// does not dial.
pub(crate) fn rustls_config() -> Result<ClientConfig, SessionsManagerClientError> {
    let provider = Arc::new(aws_lc_rs::default_provider());
    let verifier = Verifier::new_with_extra_roots(bundled_roots(), provider.clone())?;

    Ok(ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every dev and CI machine has a CA bundle, so an empty bundled set would go unnoticed until an
    /// image without one fails to build its clients. A mismatch also flags `webpki-roots` and
    /// `webpki-root-certs` drifting apart, which leaves out roots only the newer one lists.
    #[test]
    fn bundled_roots_leave_out_only_name_constrained() {
        let constrained = TLS_SERVER_ROOTS
            .iter()
            .filter(|root| root.name_constraints.is_some())
            .count();
        let bundled = bundled_roots().count();

        assert!(bundled > 0);
        assert_eq!(bundled, TLS_SERVER_ROOT_CERTS.len() - constrained);
    }
}
