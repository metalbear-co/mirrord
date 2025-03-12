use std::{io, path::Path};

use mirrord_config::external_proxy::MIRRORD_EXTPROXY_TLS_SERVER_NAME;
use mirrord_protocol::{ClientMessage, DaemonMessage};
use mirrord_tls_util::{SecureChannelError, SecureChannelSetup};
use rustls::pki_types::ServerName;
use thiserror::Error;
use tokio::{net::TcpStream, sync::mpsc};

#[derive(Error, Debug)]
pub enum ConnectionTlsError {
    #[error("failed to prepare a TLS connector: {0}")]
    SetupError(#[from] SecureChannelError),
    #[error("failed to connect with TLS: {0}")]
    ConnectionError(#[source] io::Error),
}

/// Makes a TLS connection to the external proxy within the given [`TcpStream`].
///
/// Requires that the external proxy provides a certificate with the
/// [`MIRRORD_EXTPROXY_TLS_SERVER_NAME`].
///
/// Spawns a background [`tokio::task`] that handles IO on the connection
/// and returns [`mpsc`] channels to send/receive messages.
///
/// # Params
///
/// * `stream` - [`TcpStream`] to use for establishing the TLS connection.
/// * `tls_pem` - path to a PEM file generated with [`SecureChannelSetup::try_new`].
pub async fn wrap_raw_connection(
    stream: TcpStream,
    tls_pem: &Path,
) -> Result<(mpsc::Sender<ClientMessage>, mpsc::Receiver<DaemonMessage>), ConnectionTlsError> {
    let connector = SecureChannelSetup::create_connector(tls_pem).await?;

    let server_name =
        ServerName::try_from(MIRRORD_EXTPROXY_TLS_SERVER_NAME).expect("valid hostname");

    Ok(mirrord_kube::api::wrap_raw_connection(
        connector
            .connect(server_name, stream)
            .await
            .map_err(ConnectionTlsError::ConnectionError)?,
    ))
}
