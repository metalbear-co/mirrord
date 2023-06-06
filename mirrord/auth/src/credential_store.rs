use std::{
    collections::{hash_map::Entry, HashMap},
    fmt::Debug,
    path::PathBuf,
    sync::LazyLock,
};

use kube::{Client, Resource};
use serde::{Deserialize, Serialize};
use tokio::fs;

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
    /// Currently active credential key from `credentials` field
    active_credential: String,
    /// User-specified CN to be used in all new certificates in this store. (Defaults to hostname)
    common_name: Option<String>,
    /// Credentials for operator
    /// Can be linked to several diffrent operator licenses via diffrent keys
    credentials: HashMap<String, Credentials>,
}

impl Default for CredentialStore {
    fn default() -> Self {
        CredentialStore {
            active_credential: "default".to_string(),
            common_name: None,
            credentials: HashMap::new(),
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

    pub async fn get_or_init<R>(&mut self, client: &Client) -> Result<&mut Credentials>
    where
        R: Resource + Clone + Debug,
        R: for<'de> Deserialize<'de>,
        R::DynamicType: Default,
    {
        let credentials = match self.credentials.entry(self.active_credential.clone()) {
            Entry::Vacant(entry) => entry.insert(Credentials::init()?),
            Entry::Occupied(entry) => entry.into_mut(),
        };

        if !credentials.is_ready() {
            let common_name = self
                .common_name
                .clone()
                .or_else(|| gethostname::gethostname().into_string().ok())
                .unwrap_or_default();

            credentials
                .get_client_certificate::<R>(client.clone(), &common_name)
                .await?;
        }

        Ok(credentials)
    }
}
