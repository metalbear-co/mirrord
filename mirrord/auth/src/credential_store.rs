use std::{
    collections::{hash_map::Entry, HashMap},
    fmt::Debug,
    path::PathBuf,
    sync::LazyLock,
};

use fs4::tokio::AsyncFileExt;
use kube::{Client, Resource};
use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt, SeekFrom},
};
use tracing::info;

use crate::{
    certificate::Certificate,
    credentials::Credentials,
    error::{AuthenticationError, CertificateStoreError, Result},
};

/// "~/.mirrord"
static CREDENTIALS_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    home::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".mirrord")
});

/// "~/.mirrord/credentials"
static CREDENTIALS_PATH: LazyLock<PathBuf> = LazyLock::new(|| CREDENTIALS_DIR.join("credentials"));

/// Container that is responsible for creating/loading `Credentials`
#[derive(Default, Debug, Serialize, Deserialize)]
pub struct CredentialStore {
    /// Credentials for operator
    /// Can be linked to several different operator licenses via different keys.
    credentials: HashMap<String, Credentials>,
}

impl CredentialStore {
    /// Load contents of store from file
    async fn load<R: AsyncRead + Unpin>(source: &mut R) -> Result<Self> {
        let mut buffer = Vec::new();

        source
            .read_to_end(&mut buffer)
            .await
            .map_err(CertificateStoreError::from)?;

        serde_yaml::from_slice(&buffer)
            .map_err(CertificateStoreError::from)
            .map_err(AuthenticationError::from)
    }

    /// Save contents of store to file
    async fn save<W: AsyncWrite + Unpin>(&self, writer: &mut W) -> Result<()> {
        let buffer = serde_yaml::to_string(&self).map_err(CertificateStoreError::from)?;

        writer
            .write_all(buffer.as_bytes())
            .await
            .map_err(CertificateStoreError::from)
            .map_err(AuthenticationError::from)
    }

    /// Get or create and ready up a certificate for the given `credential_name`.
    #[tracing::instrument(level = "trace", skip(self, client))]
    pub async fn get_or_init<R>(
        &mut self,
        client: &Client,
        credential_name: String,
    ) -> Result<&mut Credentials>
    where
        R: Resource + Clone + Debug,
        R: for<'de> Deserialize<'de>,
        R::DynamicType: Default,
    {
        let credentials = match self.credentials.entry(credential_name) {
            Entry::Vacant(entry) => entry.insert(Credentials::init()?),
            Entry::Occupied(entry) => entry.into_mut(),
        };

        if !credentials.is_ready() {
            let common_name = gethostname::gethostname()
                .into_string()
                .ok()
                .unwrap_or_default();

            credentials
                .get_client_certificate::<R>(client.clone(), &common_name)
                .await?;
        }

        Ok(credentials)
    }
}

/// Exposes methods to safely access [`CredentialStore`] stored in a file.
pub struct CredentialStoreSync {
    store_file: fs::File,
}

impl CredentialStoreSync {
    pub async fn open() -> Result<Self> {
        if !CREDENTIALS_DIR.exists() {
            fs::create_dir_all(&*CREDENTIALS_DIR)
                .await
                .map_err(CertificateStoreError::from)?;
        }

        let store_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&*CREDENTIALS_PATH)
            .await
            .map_err(CertificateStoreError::from)?;

        Ok(Self { store_file })
    }

    /// Try and get/create a specific client certificate.
    /// The exclusive file lock is already acquired.
    async fn access_credential<R, C, V>(
        &mut self,
        client: &Client,
        credential_name: String,
        callback: C,
    ) -> Result<V>
    where
        R: Resource + Clone + Debug,
        R: for<'de> Deserialize<'de>,
        R::DynamicType: Default,
        C: FnOnce(&mut Credentials) -> V,
    {
        let mut store = CredentialStore::load(&mut self.store_file)
            .await
            .inspect_err(|err| info!("CredentialStore Load Error {err:?}"))
            .unwrap_or_default();

        let value = callback(store.get_or_init::<R>(client, credential_name).await?);

        // Make sure the store_file's cursor is at the start of the file before sending it to save
        self.store_file
            .seek(SeekFrom::Start(0))
            .await
            .map_err(CertificateStoreError::from)?;

        store.save(&mut self.store_file).await?;

        Ok(value)
    }

    /// Get or create specific client certificate with an exclusive lock on the file.
    pub async fn get_client_certificate<R>(
        &mut self,
        client: &Client,
        credential_name: String,
    ) -> Result<Certificate>
    where
        R: Resource + Clone + Debug,
        R: for<'de> Deserialize<'de>,
        R::DynamicType: Default,
    {
        self.store_file
            .lock_exclusive()
            .map_err(CertificateStoreError::Lockfile)?;

        let result = self
            .access_credential::<R, _, Certificate>(client, credential_name, |credentials| {
                credentials.as_ref().clone()
            })
            .await;

        self.store_file
            .unlock()
            .map_err(CertificateStoreError::Lockfile)?;

        result
    }
}
