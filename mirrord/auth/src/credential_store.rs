use std::{
    collections::{HashMap, hash_map::Entry},
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
use whoami::fallible;

use crate::{
    certificate::Certificate,
    credentials::{
        Credentials,
        client::{SigningRequest, SigningResponse},
    },
    error::CredentialStoreError,
    key_pair::KeyPair,
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
    #[serde(default)]
    credentials: HashMap<String, Credentials>,
    /// Associates previously seen operator subscription ids with the [`KeyPair`]s used to generate
    /// a certification request. Enables using the same [`KeyPair`] when the operator license
    /// changes, but it belongs to the same subscription.
    #[serde(default)]
    signing_keys: HashMap<String, KeyPair>,
}

/// Information about user gathered from the local system to be shared with the operator
/// for better status reporting.
#[derive(Default, Debug)]
pub struct UserIdentity {
    /// User's name
    pub name: Option<String>,
    /// User's hostname
    pub hostname: Option<String>,
}

impl UserIdentity {
    pub fn load() -> Self {
        Self {
            // next release of whoami (v2) will have fallible types
            // so keep this Option for then :)
            name: Some(whoami::realname()),
            hostname: fallible::hostname().ok(),
        }
    }
}

impl CredentialStore {
    /// Load contents of store from file
    async fn load<R: AsyncRead + Unpin>(source: &mut R) -> Result<Self, CredentialStoreError> {
        let mut buffer = Vec::new();
        source
            .read_to_end(&mut buffer)
            .await
            .map_err(CredentialStoreError::FileAccess)?;
        serde_yaml::from_slice(&buffer).map_err(From::from)
    }

    /// Save contents of store to file
    async fn save<W: AsyncWrite + Unpin>(
        &self,
        writer: &mut W,
    ) -> Result<(), CredentialStoreError> {
        let buffer = serde_yaml::to_string(&self)?;
        writer
            .write_all(buffer.as_bytes())
            .await
            .map_err(CredentialStoreError::FileAccess)?;

        Ok(())
    }

    /// Get hostname to be used as common name in a certification request.
    fn certificate_common_name() -> String {
        let mut hostname = fallible::hostname().unwrap_or_else(|_| "localhost".to_string());

        hostname.make_ascii_lowercase();
        hostname
    }

    /// Get or create and ready up a certificate for specific operator installation.
    /// Assign the key pair used to sign the certificate with the given `operator_subscription_id`.
    ///
    /// If an expired certificate for the given `operator_fingerprint` is found, new certificate
    /// request will be signed by the same key pair. If a key pair assigned to the given
    /// `operator_subscription_id` is found, new certificate request will be signed by the same key
    /// pair.
    ///
    /// # Note
    ///
    /// Whenever we create/retrieve user's [`Credentials`], we associate the found key pair with
    /// operator's subscription id. Then, the operator's license is renewed - its fingerprint
    /// changes and we don't have any matching [`Credentials`]. But the subscription id does not
    /// change, so we look up the mapping inside [`CredentialStore`] to find the key pair we used
    /// previously for the same subscription id.
    ///
    /// Also, subscription id is accepted as an [`Option`] to make the CLI backwards compatible.
    #[tracing::instrument(level = "trace", skip(self, client))]
    pub async fn get_or_init<Old, New>(
        &mut self,
        client: &Client,
        operator_fingerprint: String,
        operator_subscription_id: Option<String>,
        support_new: bool,
    ) -> Result<&mut Credentials, CredentialStoreError>
    where
        Old: Resource + Clone + Debug,
        Old: for<'de> Deserialize<'de>,
        Old::DynamicType: Default,
        New: Clone + Debug + SigningRequest + SigningResponse + Serialize,
        New: for<'de> Deserialize<'de>,
        New::DynamicType: Default,
    {
        let credentials = match self.credentials.entry(operator_fingerprint) {
            Entry::Vacant(entry) => {
                let key_pair = operator_subscription_id
                    .as_ref()
                    .and_then(|id| self.signing_keys.get(id))
                    .cloned();
                let credentials = if support_new {
                    Credentials::init_regular::<New>(
                        client.clone(),
                        &Self::certificate_common_name(),
                        key_pair,
                    )
                    .await?
                } else {
                    Credentials::init::<Old>(
                        client.clone(),
                        &Self::certificate_common_name(),
                        key_pair,
                    )
                    .await?
                };
                entry.insert(credentials)
            }
            Entry::Occupied(entry) => {
                let credentials = entry.into_mut();

                if !credentials.is_valid() {
                    credentials
                        .refresh::<Old, New>(
                            client.clone(),
                            &Self::certificate_common_name(),
                            support_new,
                        )
                        .await?;
                }

                credentials
            }
        };

        if let Some(sub_id) = operator_subscription_id {
            self.signing_keys
                .insert(sub_id, credentials.key_pair().clone());
        }

        Ok(credentials)
    }
}

/// Exposes methods to safely access [`CredentialStore`] stored in a file.
pub struct CredentialStoreSync {
    store_file: fs::File,
}

impl CredentialStoreSync {
    pub async fn open() -> Result<Self, CredentialStoreError> {
        if !CREDENTIALS_DIR.exists() {
            fs::create_dir_all(&*CREDENTIALS_DIR)
                .await
                .map_err(CredentialStoreError::ParentDir)?;
        }

        let store_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&*CREDENTIALS_PATH)
            .await
            .map_err(CredentialStoreError::FileAccess)?;

        Ok(Self { store_file })
    }

    /// Try and get/create a specific client certificate.
    /// The exclusive file lock is already acquired.
    async fn access_credential<Old, New, C, V>(
        &mut self,
        client: &Client,
        operator_fingerprint: String,
        operator_subscription_id: Option<String>,
        support_new: bool,
        callback: C,
    ) -> Result<V, CredentialStoreError>
    where
        Old: Resource + Clone + Debug,
        Old: for<'de> Deserialize<'de>,
        Old::DynamicType: Default,
        New: Clone + Debug + SigningRequest + SigningResponse + Serialize,
        New: for<'de> Deserialize<'de>,
        New::DynamicType: Default,
        C: FnOnce(&mut Credentials) -> V,
    {
        let mut store = CredentialStore::load(&mut self.store_file)
            .await
            .inspect_err(|error| tracing::warn!(%error, "CredentialStore load failed"))
            .unwrap_or_default();

        let value = callback(
            store
                .get_or_init::<Old, New>(
                    client,
                    operator_fingerprint,
                    operator_subscription_id,
                    support_new,
                )
                .await?,
        );

        // Make sure the store_file's cursor is at the start of the file before sending it to save
        self.store_file
            .seek(SeekFrom::Start(0))
            .await
            .map_err(CredentialStoreError::FileAccess)?;
        self.store_file
            .set_len(0)
            .await
            .map_err(CredentialStoreError::FileAccess)?;

        store.save(&mut self.store_file).await?;

        Ok(value)
    }

    /// Get or create specific client certificate with an exclusive lock on the file.
    pub async fn get_client_certificate<Old, New>(
        &mut self,
        client: &Client,
        operator_fingerprint: String,
        operator_subscription_id: Option<String>,
        support_new: bool,
    ) -> Result<Certificate, CredentialStoreError>
    where
        Old: Resource + Clone + Debug,
        Old: for<'de> Deserialize<'de>,
        Old::DynamicType: Default,
        New: Clone + Debug + SigningRequest + SigningResponse + Serialize,
        New: for<'de> Deserialize<'de>,
        New::DynamicType: Default,
    {
        self.store_file
            .lock_exclusive()
            .map_err(CredentialStoreError::Lockfile)?;

        let result = self
            .access_credential::<Old, New, _, Certificate>(
                client,
                operator_fingerprint,
                operator_subscription_id,
                support_new,
                |credentials| credentials.as_ref().clone(),
            )
            .await;

        self.store_file
            .unlock()
            .map_err(CredentialStoreError::Lockfile)?;

        result
    }
}
