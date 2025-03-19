use std::{io::Write, path::Path, sync::Arc};

use pem::{EncodeConfig, LineEnding, Pem};
use rcgen::CertifiedKey;
use rustls::{server::WebPkiClientVerifier, ClientConfig, RootCertStore};
use tempfile::NamedTempFile;
use tokio_rustls::{TlsAcceptor, TlsConnector};

use crate::error::SecureChannelError;

/// Setup for secure TLS communication between two parties.
///
/// The setup includes two temporary PEM files (for the server and for the client),
/// containing certificate chains and private keys.
///
/// The PEM files are deleted when this struct is dropped.
///
/// You can use the files with [`SecureChannelSetup::create_connector`] and
/// [`SecureChannelSetup::create_acceptor`].
#[derive(Debug)]
pub struct SecureChannelSetup {
    server_pem: NamedTempFile,
    client_pem: NamedTempFile,
}

impl SecureChannelSetup {
    /// Prepares a new setup using fresh random certificates and keys.
    ///
    /// The generated certificates will contain the respective subject alternate names
    /// given to this function.
    pub fn try_new(server_name: &str, client_name: &str) -> Result<Self, SecureChannelError> {
        let root_cert = crate::generate_cert("root", None, true)?;

        Ok(Self {
            server_pem: Self::prepare_single_pem(&root_cert, server_name)?,
            client_pem: Self::prepare_single_pem(&root_cert, client_name)?,
        })
    }

    /// Creates a PEM file containing a certificate chain and a private key.
    fn prepare_single_pem(
        root: &CertifiedKey,
        server_name: &str,
    ) -> Result<NamedTempFile, SecureChannelError> {
        let cert = crate::generate_cert(server_name, Some(root), false)?;

        let pem = pem::encode_many_config(
            &[
                Pem::new("CERTIFICATE", cert.cert.der().to_vec()),
                Pem::new("CERTIFICATE", root.cert.der().to_vec()),
                Pem::new("PRIVATE KEY", cert.key_pair.serialize_der()),
            ],
            EncodeConfig::new().set_line_ending(LineEnding::LF),
        );

        let mut file =
            NamedTempFile::with_suffix(".pem").map_err(SecureChannelError::TmpFileCreateError)?;
        file.as_file_mut().write_all(pem.as_bytes())?;

        Ok(file)
    }

    /// Returns the path to the server PEM file.
    pub fn server_pem(&self) -> &Path {
        self.server_pem.path()
    }

    /// Returns the path to the client PEM file.
    pub fn client_pem(&self) -> &Path {
        self.client_pem.path()
    }

    /// Creates a [`TlsAcceptor`] from the given PEM file.
    ///
    /// This PEM file should be generated with [`SecureChannelSetup::try_new`].
    pub async fn create_acceptor(pem_path: &Path) -> Result<TlsAcceptor, SecureChannelError> {
        let cert_chain = crate::read_cert_chain(pem_path.to_path_buf()).await?;
        let key = crate::read_key_der(pem_path.to_path_buf()).await?;

        let mut roots = RootCertStore::empty();
        let root = cert_chain
            .last()
            .ok_or(SecureChannelError::NoRootCert)?
            .clone();
        roots
            .add(root)
            .map_err(SecureChannelError::InvalidRootCert)?;

        let client_verifier = WebPkiClientVerifier::builder(roots.into()).build()?;

        let tls_config = rustls::ServerConfig::builder()
            .with_client_cert_verifier(client_verifier)
            .with_single_cert(cert_chain, key)
            .map_err(SecureChannelError::InvalidCertChain)?;

        Ok(TlsAcceptor::from(Arc::new(tls_config)))
    }

    /// Creates a [`TlsConnector`] from the given PEM file.
    ///
    /// This PEM file should be generated with [`SecureChannelSetup::try_new`].
    pub async fn create_connector(pem_path: &Path) -> Result<TlsConnector, SecureChannelError> {
        let chain = crate::read_cert_chain(pem_path.to_path_buf()).await?;
        let private_key = crate::read_key_der(pem_path.to_path_buf()).await?;

        let root = chain.last().ok_or(SecureChannelError::NoRootCert)?.clone();

        let mut root_cert_store = RootCertStore::empty();
        root_cert_store
            .add(root)
            .map_err(SecureChannelError::InvalidRootCert)?;

        let tls_config = ClientConfig::builder()
            .with_root_certificates(root_cert_store)
            .with_client_auth_cert(chain, private_key)
            .map_err(SecureChannelError::InvalidCertChain)?;

        Ok(TlsConnector::from(Arc::new(tls_config)))
    }
}
