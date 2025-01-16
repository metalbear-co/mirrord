use mirrord_protocol::tcp::{HTTP_CHUNKED_RESPONSE_VERSION, HTTP_FRAMED_VERSION};

/// Determines how [`IncomingProxy`](crate::proxies::incoming::IncomingProxy) should send HTTP
/// responses.
#[derive(Debug, Clone, Copy, Default)]
pub enum ResponseMode {
    /// [`LayerTcpSteal::HttpResponseChunked`](mirrord_protocol::tcp::LayerTcpSteal::HttpResponseChunked)
    Chunked,
    /// [`LayerTcpSteal::HttpResponseFramed`](mirrord_protocol::tcp::LayerTcpSteal::HttpResponseFramed)
    Framed,
    /// [`LayerTcpSteal::HttpResponse`](mirrord_protocol::tcp::LayerTcpSteal::HttpResponse)
    #[default]
    Basic,
}

impl From<&semver::Version> for ResponseMode {
    fn from(value: &semver::Version) -> Self {
        if HTTP_CHUNKED_RESPONSE_VERSION.matches(value) {
            Self::Chunked
        } else if HTTP_FRAMED_VERSION.matches(value) {
            Self::Framed
        } else {
            Self::Basic
        }
    }
}
