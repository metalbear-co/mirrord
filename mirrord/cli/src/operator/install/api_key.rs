//! Obtains an operator API key for the organization of the user logged in with `mirrord login`,
//! so that the key goes from the backend into the cluster without the user handling it.

use reqwest::StatusCode;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use url::Url;

use super::error::OperatorInstallError;
use crate::{
    config::LoginArgs,
    data::{AuthStore, StoredLoginToken},
    login,
};

const CREATE_KEY_PATH: &str = "api/v1/cli/operator-api-key";

/// App page where admins revoke the organization's operator API key.
const SETTINGS_PATH: &str = "settings";

/// The login token for `app_url`, a base from [`login::backend_base_url`], logging the user in
/// first when there is none.
pub(super) async fn login_token(app_url: &Url) -> Result<StoredLoginToken, OperatorInstallError> {
    let issuer = login::issuer(app_url);
    if let Some(token) = AuthStore::login_token(issuer)
        .await
        .map_err(OperatorInstallError::LoginStore)?
    {
        return Ok(token);
    }

    login::login_command(LoginArgs {
        url: app_url.clone(),
    })
    .await
    .map_err(|error| OperatorInstallError::Login(Box::new(error)))?;

    AuthStore::login_token(issuer)
        .await
        .map_err(OperatorInstallError::LoginStore)?
        .ok_or(OperatorInstallError::NotLoggedIn)
}

/// Where admins revoke the organization's operator API key.
pub(super) fn settings_url(app_url: &Url) -> Url {
    // `app_url` is an http(s) base, which joins any relative path.
    app_url
        .join(SETTINGS_PATH)
        .expect("an http(s) base joins a relative path")
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
    app_url: &Url,
    token: &StoredLoginToken,
    name: Option<&str>,
) -> Result<SecretString, OperatorInstallError> {
    // `app_url` is an http(s) base, which joins any relative path.
    let url = app_url
        .join(CREATE_KEY_PATH)
        .expect("an http(s) base joins a relative path");
    let response = http
        .post(url)
        .bearer_auth(token.token.expose_secret())
        .json(&CreateKeyRequest { name })
        .send()
        .await
        .map_err(OperatorInstallError::ApiKeyRequest)?;

    match response.status() {
        status if status.is_success() => response
            .json::<CreatedKey>()
            .await
            .map(|created| SecretString::from(created.key))
            .map_err(OperatorInstallError::ApiKeyRequest),
        StatusCode::UNAUTHORIZED => Err(OperatorInstallError::LoginExpired),
        StatusCode::FORBIDDEN => Err(OperatorInstallError::NotAdmin {
            email: token.email.clone(),
        }),
        StatusCode::CONFLICT => Err(OperatorInstallError::ApiKeyExists {
            settings_url: settings_url(app_url),
        }),
        status => Err(OperatorInstallError::ApiKeyFailed {
            status,
            body: response.text().await.unwrap_or_default(),
        }),
    }
}
