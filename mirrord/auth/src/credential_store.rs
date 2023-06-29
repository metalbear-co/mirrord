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
    /// User-specified CN to be used in all new certificates in this store. (Defaults to hostname)
    common_name: Option<String>,
    /// Credentials for operator
    /// Can be linked to several diffrent operator licenses via diffrent keys
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

    /// Get or create and ready up a certificate at `active_credential` slot
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

/// A `CredentialStore` but saved loaded from file and saved with exclusive lock on the file
pub struct CredentialStoreSync;

impl CredentialStoreSync {
    /// Try and get/create a client certificate used for `get_client_certificate` once a file lock
    /// is created
    async fn try_get_client_certificate<R>(
        client: &Client,
        credential_name: String,
        store_file: &mut fs::File,
    ) -> Result<Vec<u8>>
    where
        R: Resource + Clone + Debug,
        R: for<'de> Deserialize<'de>,
        R::DynamicType: Default,
    {
        let mut store = CredentialStore::load(store_file)
            .await
            .inspect_err(|err| info!("CredentialStore Load Error {err:?}"))
            .unwrap_or_default();

        let certificate_der = store
            .get_or_init::<R>(client, credential_name)
            .await?
            .as_ref()
            .encode_der()
            .map_err(AuthenticationError::Pem)?;

        // Make sure the store_file's cursor is at the start of the file before sending it to save
        store_file
            .seek(SeekFrom::Start(0))
            .await
            .map_err(CertificateStoreError::from)?;

        store.save(store_file).await?;

        Ok(certificate_der)
    }

    /// Get client certificate while keeping an exclusive lock on `CREDENTIALS_PATH` (and releasing
    /// it regrading of result)
    pub async fn get_client_certificate<R>(
        client: &Client,
        credential_name: String,
    ) -> Result<Vec<u8>>
    where
        R: Resource + Clone + Debug,
        R: for<'de> Deserialize<'de>,
        R::DynamicType: Default,
    {
        if !CREDENTIALS_DIR.exists() {
            fs::create_dir_all(&*CREDENTIALS_DIR)
                .await
                .map_err(CertificateStoreError::from)?;
        }

        let mut store_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&*CREDENTIALS_PATH)
            .await
            .map_err(CertificateStoreError::from)?;

        store_file
            .lock_exclusive()
            .map_err(CertificateStoreError::Lockfile)?;

        let result =
            Self::try_get_client_certificate::<R>(client, credential_name, &mut store_file).await;

        store_file
            .unlock()
            .map_err(CertificateStoreError::Lockfile)?;

        result
    }
}
