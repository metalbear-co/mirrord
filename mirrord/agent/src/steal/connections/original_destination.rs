use std::{io::Result, net::SocketAddr};

use http::Uri;
use mirrord_tls_util::MaybeTls;
use tokio::net::TcpStream;

use crate::steal::tls::handler::PassThroughTlsConnector;

/// Original destination of a stolen HTTPS request.
///
/// Used in [`FilteredStealTask`](super::filtered::FilteredStealTask).
#[derive(Clone, Debug)]
pub struct OriginalDestination {
    address: SocketAddr,
    connector: Option<PassThroughTlsConnector>,
}

impl OriginalDestination {
    /// Creates a new instance.
    ///
    /// # Params
    ///
    /// * `address` - address of the HTTP server.
    /// * `connector` - optional TLS connector. If given, it means that the original HTTP connection
    ///   was wrapped in TLS. We should pass the requests with TLS as well.
    pub fn new(address: SocketAddr, connector: Option<PassThroughTlsConnector>) -> Self {
        Self { address, connector }
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }

    pub fn connector(&self) -> Option<&PassThroughTlsConnector> {
        self.connector.as_ref()
    }

    /// Makes a connection to the server.
    ///
    /// Given [`Uri`] will be used in case we need a TLS connection and we don't have the original
    /// SNI (we need some server name to connect with TLS).
    pub async fn connect(&self, request_uri: &Uri) -> Result<MaybeTls> {
        let stream = TcpStream::connect(self.address).await?;

        match self.connector.as_ref() {
            Some(connector) => {
                let stream = connector
                    .connect(self.address.ip(), Some(request_uri), stream)
                    .await?;
                Ok(MaybeTls::Tls(stream))
            }
            None => Ok(MaybeTls::NoTls(stream)),
        }
    }
}
