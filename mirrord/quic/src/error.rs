use std::io;

use thiserror::Error;

/// Errors that can occur when building the QUIC configuration for either end.
#[derive(Debug, Error)]
pub enum QuicSetupError {
    /// The operator certificate PEM could not be parsed.
    #[error("failed to parse the operator certificate: {0}")]
    MalformedOperatorCert(rustls::pki_types::pem::Error),
    /// The operator certificate PEM contained no certificate.
    #[error("the operator certificate PEM contains no certificate")]
    NoOperatorCert,
    /// The operator private key PEM could not be parsed.
    #[error("failed to parse the operator private key: {0}")]
    MalformedOperatorKey(rustls::pki_types::pem::Error),
    /// The generated agent certificate produced a key rustls cannot use.
    #[error("failed to encode the generated agent private key: {0}")]
    MalformedAgentKey(String),
    /// We failed to generate the agent's ephemeral certificate.
    #[error("failed to generate the agent certificate: {0}")]
    CertGeneration(#[from] rcgen::Error),
    /// rustls rejected the configuration, e.g. the certificate and key do not match.
    #[error("failed to build the TLS configuration: {0}")]
    Tls(#[from] rustls::Error),
    /// The TLS configuration cannot be used with QUIC, e.g. it allows a version older than 1.3.
    #[error("the TLS configuration is not usable with QUIC: {0}")]
    NoQuicSupport(#[from] quinn::crypto::rustls::NoInitialCipherSuite),
    /// The client verifier could not be built from the operator certificate.
    #[error("failed to build the client certificate verifier: {0}")]
    ClientVerifier(#[from] rustls::server::VerifierBuilderError),
}

/// Errors that can occur when establishing a data stream for an intercepted connection.
#[derive(Debug, Error)]
pub enum DataStreamError {
    /// The connection was lost while the stream was being established.
    #[error("failed to establish the data stream: {0}")]
    Connection(#[from] quinn::ConnectionError),
    /// The stream was closed or errored while the header was being exchanged.
    #[error("failed to exchange the data stream header: {0}")]
    Io(#[from] io::Error),
    /// The peer opened a data stream for something this build does not know how to carry. Only
    /// reachable if the negotiated version is wrong, since the kind is covered by version
    /// negotiation.
    #[error("the peer opened a data stream of unknown kind {0}")]
    UnknownKind(u8),
}

/// Errors that can occur when establishing the control stream.
#[derive(Debug, Error)]
pub enum ControlStreamError {
    /// The peer never opened the stream, or the connection was lost while establishing it.
    #[error("failed to establish the control stream: {0}")]
    Connection(#[from] quinn::ConnectionError),
    /// The stream was closed or errored while the header was being exchanged.
    #[error("failed to exchange the control stream header: {0}")]
    Io(#[from] io::Error),
    /// The peer's first bytes were not a control stream header, so it is speaking something other
    /// than this transport.
    #[error("the peer did not send a mirrord control stream header")]
    BadMagic,
}
