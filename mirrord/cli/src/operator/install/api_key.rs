//! Obtains an operator API key for the organization of the user logged in with `mirrord login`,
//! so that the key goes from the backend into the cluster without the user handling it.

use reqwest::{StatusCode, header::AUTHORIZATION};
use serde::{Deserialize, Serialize};

use super::error::OperatorInstallError;
use crate::{
    config::LoginArgs,
    data::{AuthStore, StoredLoginToken},
    login,
};

const CREATE_KEY_PATH: &str = "/api/v1/cli/operator-api-key";

/// An operator API key.
///
/// Deliberately not [`Debug`], to keep the key out of logs.
pub(super) struct ApiKey(String);

impl ApiKey {
    pub(super) fn as_str(&self) -> &str {
        &self.0
    }
}

/// The login token for `app_url`, logging the user in first when there is none.
pub(super) async fn login_token(app_url: &str) -> Result<StoredLoginToken, OperatorInstallError> {
    let issuer = app_url.trim_end_matches('/');
    if let Some(token) = AuthStore::login_token(issuer)
        .await
        .map_err(OperatorInstallError::LoginStore)?
    {
        return Ok(token);
    }

    let url = app_url
        .parse::<url::Url>()
        .map_err(|error| OperatorInstallError::Login(Box::new(login::LoginError::from(error))))?;
    login::login_command(LoginArgs { url })
        .await
        .map_err(|error| OperatorInstallError::Login(Box::new(error)))?;

    AuthStore::login_token(issuer)
        .await
        .map_err(OperatorInstallError::LoginStore)?
        .ok_or(OperatorInstallError::NotLoggedIn)
}

#[derive(Serialize)]
struct CreateKeyRequest<'a> {
    /// Shown next to the key in the organization's settings, to tell which cluster uses it.
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<&'a str>,
}

#[derive(Deserialize)]
struct CreatedKey {
    key: String,
}

/// Creates the operator API key of the organization that `token` belongs to.
pub(super) async fn create(
    http: &reqwest::Client,
    app_url: &str,
    token: &StoredLoginToken,
    name: Option<&str>,
) -> Result<ApiKey, OperatorInstallError> {
    let response = http
        .post(format!(
            "{}{CREATE_KEY_PATH}",
            app_url.trim_end_matches('/')
        ))
        .header(AUTHORIZATION, format!("Bearer {}", token.token.as_str()))
        .json(&CreateKeyRequest { name })
        .send()
        .await
        .map_err(OperatorInstallError::ApiKeyRequest)?;

    match response.status() {
        status if status.is_success() => response
            .json::<CreatedKey>()
            .await
            .map(|created| ApiKey(created.key))
            .map_err(OperatorInstallError::ApiKeyRequest),
        StatusCode::UNAUTHORIZED => Err(OperatorInstallError::LoginExpired),
        StatusCode::FORBIDDEN => Err(OperatorInstallError::NotAdmin {
            email: token.email.clone(),
        }),
        StatusCode::CONFLICT => Err(OperatorInstallError::ApiKeyExists),
        status => Err(OperatorInstallError::ApiKeyFailed {
            status,
            body: response.text().await.unwrap_or_default(),
        }),
    }
}
