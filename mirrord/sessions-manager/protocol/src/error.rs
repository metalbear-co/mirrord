use http::Uri;
use url::Url;

#[derive(thiserror::Error, Debug)]
pub enum SessionsManagerProtocolError {
    #[error("Invalid dataplane endpoint assigned: {0}")]
    InvalidDataPlaneEndpointRecv(Uri),
    #[error("Invalid dataplane endpoint resolved: {0}")]
    InvalidDataPlaneEndpointRes(Url),
}
