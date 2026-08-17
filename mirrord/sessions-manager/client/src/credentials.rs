use reqwest::header::HeaderMap;

use crate::error::SessionsManagerClientError;

pub trait CredentialProvider: Send + Sync {
    fn headers(&self) -> Result<HeaderMap, SessionsManagerClientError>;
}

#[derive(Default)]
pub(crate) struct NoCredentials;

impl CredentialProvider for NoCredentials {
    fn headers(&self) -> Result<HeaderMap, SessionsManagerClientError> {
        Ok(HeaderMap::new())
    }
}
