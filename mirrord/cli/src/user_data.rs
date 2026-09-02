use std::{
    env::home_dir,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::LazyLock,
};

use atomic_write_file::AtomicWriteFile;
use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use tokio::task;
use tracing::trace;
use uuid::Uuid;

/// "~/.mirrord"
static DATA_STORE_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".mirrord")
});

/// "~/.mirrord/data.json"
static DATA_STORE_PATH: LazyLock<PathBuf> = LazyLock::new(|| DATA_STORE_DIR.join("data.json"));

/// Data that we store in the user's machine at `~/.mirrord/data.json` that might be used
/// for a variety of purposes.
///
/// Missing fields use their defaults so older files remain compatible. Loading rewrites only data
/// that needs migration or replacement, keeping the file current without rewriting every run.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct UserData {
    /// Amount of times this user has run mirrord.
    #[serde(default)]
    session_count: u32,

    /// Helps us keep track of unique users for analytics when telemetry is enabled.
    ///
    /// Must use custom `default =`, since the default is [`Uuid::nil`].
    ///
    /// When deserialziing a [`UserData`] file, the `machine_id` might not be present, but
    /// we don't want `serde` to error and overwrite the other [`UserData`] fields with
    /// default values.
    #[serde(default = "Uuid::new_v4")]
    machine_id: Uuid,

    /// True if the user has used the `mirrord wizard` command enough to be considered a returning
    /// user. This update is triggered when the wizard gets a request on the cluster-details
    /// endpoint, which happens when the user starts the flow to create a config file.
    #[serde(default)]
    is_returning_wizard: bool,
}

impl Default for UserData {
    fn default() -> Self {
        Self {
            session_count: 0,
            machine_id: Uuid::new_v4(),
            is_returning_wizard: false,
        }
    }
}

impl UserData {
    /// Creates `UserData` from the default file path (`DATA_STORE_PATH`).
    pub(crate) async fn from_default_path() -> io::Result<Self> {
        Self::from_path(DATA_STORE_PATH.as_path()).await
    }

    async fn from_path(path: &Path) -> io::Result<Self> {
        Self::update_at_path(path, |_| {}).await
    }

    /// Increases the session count by one and returns the number.
    pub(crate) async fn bump_session_count(&mut self) -> io::Result<u32> {
        *self = Self::update_at_path(DATA_STORE_PATH.as_path(), |data| {
            data.session_count += 1;
        })
        .await?;

        Ok(self.session_count)
    }

    /// Updates user data file to indicate that user has used the Wizard.
    pub(crate) async fn update_is_returning_wizard(&mut self) -> io::Result<()> {
        *self = Self::update_at_path(DATA_STORE_PATH.as_path(), |data| {
            data.is_returning_wizard = true;
        })
        .await?;

        Ok(())
    }

    pub(crate) fn is_returning_wizard(&self) -> bool {
        self.is_returning_wizard
    }

    pub(crate) fn machine_id(&self) -> Uuid {
        self.machine_id
    }

    async fn update_at_path(
        path: &Path,
        update: impl FnOnce(&mut Self) + Send + 'static,
    ) -> io::Result<Self> {
        let path = path.to_owned();
        task::spawn_blocking(move || {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)?;
            }

            // Atomic replacement changes the data file's inode, so writers coordinate through a
            // sidecar whose identity remains stable across commits.
            let lock_path = path.with_extension("lock");
            let lock_file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(lock_path)?;
            lock_file.lock_exclusive()?;

            let previous = match fs::read(&path) {
                Ok(contents) => Some(contents),
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(error),
            };
            let mut user_data = previous
                .as_deref()
                .map(|contents| {
                    Self::deserialize(contents).unwrap_or_else(|error| {
                        trace!(
                            %error,
                            "Could not deserialize `UserData`; replacing it with defaults"
                        );
                        Self::default()
                    })
                })
                .unwrap_or_default();

            update(&mut user_data);

            let contents = serde_json::to_vec(&user_data).map_err(io::Error::other)?;
            if previous.as_deref() != Some(contents.as_slice()) {
                let mut store_file = AtomicWriteFile::open(&path)?;
                store_file.write_all(&contents)?;
                store_file.commit()?;
            }

            Ok(user_data)
        })
        .await
        .map_err(io::Error::other)?
    }

    fn deserialize(contents: &[u8]) -> io::Result<Self> {
        serde_json::from_slice(contents)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
    }
}

#[cfg(test)]
mod tests {
    use tokio::{fs, join};

    use super::*;

    #[tokio::test]
    async fn loading_existing_data_does_not_rewrite_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("data.json");
        let original = UserData {
            session_count: 7,
            machine_id: Uuid::nil(),
            is_returning_wizard: true,
        };
        let original = serde_json::to_vec(&original).unwrap();
        fs::write(&path, &original).await.unwrap();

        let data = UserData::from_path(&path).await.unwrap();

        assert_eq!(data.session_count, 7);
        assert_eq!(fs::read(path).await.unwrap(), original);
    }

    #[tokio::test]
    async fn loading_valid_legacy_data_adds_missing_fields() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("data.json");
        fs::write(&path, br#"{"session_count":7}"#).await.unwrap();

        let data = UserData::from_path(&path).await.unwrap();

        assert_eq!(
            fs::read(path).await.unwrap(),
            serde_json::to_vec(&data).unwrap()
        );
    }

    #[tokio::test]
    async fn loading_malformed_data_replaces_it_with_defaults() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("data.json");
        let original = br#"{"session_count": "unfinished""#;
        fs::write(&path, original).await.unwrap();

        let data = UserData::from_path(&path).await.unwrap();

        assert_eq!(data.session_count, 0);
        assert_eq!(
            fs::read(path).await.unwrap(),
            serde_json::to_vec(&data).unwrap()
        );
    }

    #[tokio::test]
    async fn concurrent_updates_preserve_both_changes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("data.json");

        let first = UserData::update_at_path(&path, |data| data.session_count += 1);
        let second = UserData::update_at_path(&path, |data| data.session_count += 1);
        let (first, second) = join!(first, second);
        first.unwrap();
        second.unwrap();

        let data = UserData::from_path(&path).await.unwrap();
        assert_eq!(data.session_count, 2);
    }
}
