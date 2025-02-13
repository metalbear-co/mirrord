use crate::http::HttpVersion;

pub trait HttpVersionExt: Sized {
    fn from_alpn_name(bytes: &[u8]) -> Option<Self>;
}

impl HttpVersionExt for HttpVersion {
    fn from_alpn_name(bytes: &[u8]) -> Option<Self> {
        match bytes {
            b"h2" => Some(Self::V2),
            b"http/1.0" | b"http/1.1" => Some(Self::V1),
            _ => None,
        }
    }
}
