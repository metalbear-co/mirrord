//! `cloud/*`: whether the developer uses MetalBear Cloud.
//!
//! Serverless targets run through MetalBear Cloud without an operator, often without a kube
//! context at all, so for these developers a missing operator or cluster is expected rather than
//! something to fix. The frontend hides the operator and Kubernetes guidance while this reports a
//! `mirrord login`. Their own sessions already show with the other local ones, since every
//! `mirrord exec` reports to the session monitor regardless of its target.

use serde::Serialize;
use tracing::warn;

use crate::{
    data::{AuthStore, StoredLogin},
    login::issuer,
};

#[derive(Serialize)]
pub(super) struct CloudLoginResponse {
    /// The developer `mirrord login` signed in to the MetalBear Cloud set by
    /// `MIRRORD_METALBEAR_CLOUD_URL`, absent when there is no unexpired login or it can't be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    email: Option<String>,
}

pub(super) async fn cloud_login() -> axum::Json<CloudLoginResponse> {
    let email = async {
        let cloud_url = StoredLogin::cloud_url().map_err(|error| error.to_string())?;
        AuthStore::login_token(issuer(&cloud_url))
            .await
            .map_err(|error| error.to_string())
    }
    .await
    .unwrap_or_else(|error| {
        // Failing to tell only keeps the operator guidance on screen, so it's not an error.
        warn!(%error, "Could not read the mirrord login");
        None
    })
    .map(|login| login.email);

    axum::Json(CloudLoginResponse { email })
}
