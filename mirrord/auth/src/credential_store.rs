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

    async fn save<W: AsyncWrite + Unpin>(&self, writer: &mut W) -> Result<()> {
        let buffer = serde_yaml::to_string(&self).map_err(CertificateStoreError::from)?;

        writer
            .write_all(buffer.as_bytes())
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

pub struct CredentialStoreSync;

impl CredentialStoreSync {
    pub async fn try_get_client_certificate<R>(
        client: &Client,
        store_file: &mut fs::File,
    ) -> Result<Vec<u8>>
    where
        R: Resource + Clone + Debug,
        R: for<'de> Deserialize<'de>,
        R::DynamicType: Default,
    {
        let mut store = CredentialStore::load(store_file)
            .await
            .inspect_err(|err| eprintln!("CredentialStore Load Error {err:?}"))
            .unwrap_or_default();

        store_file
            .seek(SeekFrom::Start(0))
            .await
            .map_err(CertificateStoreError::from)?;

        let certificate_der = store
            .get_or_init::<R>(client)
            .await?
            .as_ref()
            .encode_der()
            .map_err(AuthenticationError::Pem)?;

        store.save(store_file).await?;

        Ok(certificate_der)
    }

    pub async fn get_client_certificate<R>(client: &Client) -> Result<Vec<u8>>
    where
        R: Resource + Clone + Debug,
        R: for<'de> Deserialize<'de>,
        R::DynamicType: Default,
    {
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

        let result = Self::try_get_client_certificate::<R>(client, &mut store_file).await;

        store_file
            .unlock()
            .map_err(CertificateStoreError::Lockfile)?;

        result
    }
}
