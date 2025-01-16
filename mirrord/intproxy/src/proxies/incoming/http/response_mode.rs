use mirrord_protocol::tcp::{HTTP_CHUNKED_RESPONSE_VERSION, HTTP_FRAMED_VERSION};

#[derive(Debug, Clone, Copy, Default)]
pub enum ResponseMode {
    Chunked,
    Framed,
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
