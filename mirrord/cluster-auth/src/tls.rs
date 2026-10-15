//! TLS configuration for remote Kubernetes cluster connections.
//!
//! # Why Custom TLS Configuration?
//!
//! We need custom TLS handling instead of using kube-rs's default because:
//!
//! ## Problem: Server-Requested Client Certificates
//!
//! Some Kubernetes API servers (fo example minikube, some on-prem clusters) **request**
//! client certificates during the TLS handshake, even when they also accept bearer
//! token authentication. This is configured via the API server's `--client-ca-file`
//! flag which enables optional client cert auth.
//!
//! The handshake flow:
//! 1. Client sends ClientHello
//! 2. Server responds with ServerHello + Certificate
//! 3. Server sends CertificateRequest (asking for client cert)
//! 4. Client must respond - but we only have a bearer token, not a cert
//!
//! ## The TLS Protocol Solution
//!
//! When a TLS 1.3 server requests a client certificate but the client doesn't
//! have one, the correct behavior per [RFC 8446 §4.4.2][rfc] is to send an
//! **empty Certificate message**. The server then decides whether to continue
//! without client authentication or abort with a `certificate_required` alert.
//!
//! Note that rustls's `with_no_client_auth()` won't work here. Itconfigures the
//! client to skip client authentication entirely, which causes handshake failures
//! when the server sends a `CertificateRequest`.
//!
//! ## Our Solution: `EmptyClientCertResolver`
//!
//! We implement a custom `ResolvesClientCert` that explicitly returns `None`,
//! telling rustls to send an empty certificate chain. The flow becomes:
//!
//! 1. Server requests client certificate during TLS handshake
//! 2. Our resolver returns `None`, sending an empty certificate chain (valid TLS)
//! 3. TLS handshake completes successfully
//! 4. Client sends HTTP request with `Authorization: Bearer <token>` header
//! 5. Server authenticates via bearer token instead of client cert
//!
//! ## When Client Certificates ARE Needed (mTLS)
//!
//! For clusters requiring mutual TLS (mTLS), we support loading client certificates
//! from the cluster Secret. In this case, we use rustls's built-in
//! `with_client_auth_cert()` method.
//!
//! [rfc]: https://datatracker.ietf.org/doc/html/rfc8446#section-4.4.2

use std::{io::Cursor, sync::Arc};

use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use rustls::{
    ClientConfig, RootCertStore, client::ResolvesClientCert, pki_types::CertificateDer,
    sign::CertifiedKey,
};
use rustls_pemfile::certs;

use crate::{
    credentials::ClusterCredentials,
    error::{ClusterAuthError, Result, context},
};

/// Trait for building TLS configurations.
///
/// This abstraction allows swapping TLS implementations for testing
/// or alternative TLS stacks.
pub trait TlsConfigBuilder: Send + Sync {
    /// Build a rustls `ClientConfig` for connecting to the given cluster.
    fn build(&self, creds: &ClusterCredentials) -> Result<ClientConfig>;
}

/// Default TLS configuration builder.
///
/// Handles:
/// - Custom CA certificates from cluster Secrets
/// - System root CAs as fallback
/// - Client certificate authentication (mTLS)
/// - Empty client cert chain for bearer token auth (see module docs)
#[derive(Debug, Clone, Default)]
pub struct DefaultTlsBuilder;

impl TlsConfigBuilder for DefaultTlsBuilder {
    fn build(&self, creds: &ClusterCredentials) -> Result<ClientConfig> {
        let root_store = self.build_root_store(creds)?;

        if creds.has_client_cert() {
            self.build_with_client_cert(creds, root_store)
        } else {
            // Use empty resolver for bearer token auth
            // (see module docs for why this is necessary)
            Ok(self.build_with_empty_resolver(root_store))
        }
    }
}

impl DefaultTlsBuilder {
    /// Build the root certificate store from CA data or system roots.
    fn build_root_store(&self, creds: &ClusterCredentials) -> Result<RootCertStore> {
        let mut root_store = RootCertStore::empty();

        if let Some(ca_data) = &creds.ca_data {
            let ca_bytes = BASE64.decode(ca_data).map_err(|e| {
                ClusterAuthError::ConfigError(context(
                    format!("Failed to decode CA data for cluster {}", creds.name),
                    e,
                ))
            })?;

            let certs_result: Vec<CertificateDer<'static>> = certs(&mut Cursor::new(&ca_bytes))
                .filter_map(|r| r.ok())
                .collect();

            if certs_result.is_empty() {
                tracing::warn!(
                    cluster = %creds.name,
                    "No valid certificates found in CA data"
                );
            }

            for cert in certs_result {
                root_store.add(cert).map_err(|e| {
                    ClusterAuthError::ConfigError(context(
                        format!("Failed to add CA certificate for cluster {}", creds.name),
                        e,
                    ))
                })?;
            }

            tracing::debug!(
                cluster = %creds.name,
                num_roots = root_store.len(),
                "Configured with custom CA certificates"
            );
        } else {
            root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            tracing::debug!(
                cluster = %creds.name,
                "Using system root certificates"
            );
        }

        Ok(root_store)
    }

    /// Build config with client certificate for mTLS.
    fn build_with_client_cert(
        &self,
        creds: &ClusterCredentials,
        root_store: RootCertStore,
    ) -> Result<ClientConfig> {
        // PEM data was already validated on credential load
        let cert_pem = creds.client_cert_pem.as_ref().ok_or_else(|| {
            ClusterAuthError::ConfigError(
                format!("Missing client certificate for cluster {}", creds.name).into(),
            )
        })?;

        let key_pem = creds.client_key_pem.as_ref().ok_or_else(|| {
            ClusterAuthError::ConfigError(
                format!("Missing client key for cluster {}", creds.name).into(),
            )
        })?;

        let cert_chain: Vec<_> = certs(&mut Cursor::new(cert_pem))
            .filter_map(|r| r.ok())
            .collect();

        let key = rustls_pemfile::private_key(&mut Cursor::new(key_pem))
            .ok()
            .flatten()
            .ok_or_else(|| {
                ClusterAuthError::ConfigError(
                    format!("Failed to parse private key for cluster {}", creds.name).into(),
                )
            })?;

        let num_certs = cert_chain.len();
        let config = ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_client_auth_cert(cert_chain, key)
            .map_err(|e| {
                ClusterAuthError::ConfigError(context(
                    format!(
                        "Failed to configure client certificate for cluster {}",
                        creds.name
                    ),
                    e,
                ))
            })?;

        tracing::debug!(
            cluster = %creds.name,
            num_certs = num_certs,
            "Configured client certificate for mTLS"
        );

        Ok(config)
    }

    /// Build config with empty client cert resolver for bearer token auth.
    ///
    /// See module-level documentation for why this is necessary.
    fn build_with_empty_resolver(&self, root_store: RootCertStore) -> ClientConfig {
        ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_client_cert_resolver(Arc::new(EmptyClientCertResolver))
    }
}

/// Empty client certificate resolver for bearer token authentication.
///
/// When a Kubernetes API server requests a client certificate during TLS
/// handshake (via CertificateRequest message), this resolver returns `None`,
/// which causes rustls to send an empty certificate chain.
///
/// This is valid TLS 1.3 behavior - the server will then authenticate the
/// client via the bearer token in the HTTP Authorization header instead.
///
/// See module-level documentation for the full explanation.
#[derive(Debug, Clone)]
struct EmptyClientCertResolver;

impl ResolvesClientCert for EmptyClientCertResolver {
    fn resolve(
        &self,
        _root_hint_subjects: &[&[u8]],
        _sigschemes: &[rustls::SignatureScheme],
    ) -> Option<Arc<CertifiedKey>> {
        // Return None to send empty certificate chain
        // This allows bearer token auth to work on servers that request client certs
        None
    }

    fn has_certs(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_resolver_returns_none() {
        let resolver = EmptyClientCertResolver;
        assert!(!resolver.has_certs());
        assert!(resolver.resolve(&[], &[]).is_none());
    }
}
