use std::{collections::BTreeMap, fmt::Debug, path::PathBuf, sync::LazyLock};

pub use bcder;
use kube::{Client, Resource};
pub use pem;
use serde::{Deserialize, Serialize};
use tokio::fs;
pub use x509_certificate;

use crate::{
    credentials::Credentials,
    error::{AuthenticationError, CertificateStoreError, Result},
};

static CREDENTIALS_PATH: LazyLock<PathBuf> = LazyLock::new(|| {
    home::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".mirrord/credentials")
});

#[derive(Debug, Serialize, Deserialize)]
pub struct CredentialStore {
    active: String,
    client_credentials: BTreeMap<String, Credentials>,
}

impl Default for CredentialStore {
    fn default() -> Self {
        CredentialStore {
            active: "default".to_string(),
            client_credentials: BTreeMap::new(),
        }
    }
}

impl CredentialStore {
    pub async fn load() -> Result<Self> {
        let buffer = fs::read(&*CREDENTIALS_PATH)
            .await
            .map_err(CertificateStoreError::from)?;

        serde_yaml::from_slice(&buffer)
            .map_err(CertificateStoreError::from)
            .map_err(AuthenticationError::from)
    }

    pub async fn save(&self) -> Result<()> {
        let buffer = serde_yaml::to_string(&self).map_err(CertificateStoreError::from)?;

        fs::write(&*CREDENTIALS_PATH, &buffer)
            .await
            .map_err(CertificateStoreError::from)
            .map_err(AuthenticationError::from)
    }

    pub async fn get_or_init<R>(&mut self, client: &Client, cn: &str) -> Result<&mut Credentials>
    where
        R: Resource + Clone + Debug,
        R: for<'de> Deserialize<'de>,
        R::DynamicType: Default,
    {
        if !self.client_credentials.contains_key(&self.active) {
            self.client_credentials
                .insert(self.active.clone(), Credentials::init()?);
        }

        let credentials = self
            .client_credentials
            .get_mut(&self.active)
            .expect("Unreachable");

        if !credentials.is_ready() {
            credentials
                .get_client_certificate::<R>(client.clone(), cn)
                .await?;
        }

        Ok(credentials)
    }
}
