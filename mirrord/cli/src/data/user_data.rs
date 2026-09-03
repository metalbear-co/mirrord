use std::{
    io,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{default_path, update_at_path};

/// "~/.mirrord/data.json"
static DATA_STORE_PATH: LazyLock<PathBuf> = LazyLock::new(|| default_path("data.json"));

/// Data that we store in the user's machine at `~/.mirrord/data.json` that might be used
/// for a variety of purposes.
///
/// Missing fields use their defaults so older files remain compatible. Loading rewrites only data
/// that needs migration or replacement, keeping the file current without rewriting every run.
#[derive(Clone, Debug, Serialize, Deserialize)]
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
        update_at_path(path, |_| {}).await
    }

    /// Increases the session count by one and returns the number.
    pub(crate) async fn bump_session_count(&mut self) -> io::Result<u32> {
        *self = update_at_path(DATA_STORE_PATH.as_path(), |data: &mut Self| {
            data.session_count += 1;
        })
        .await?;

        Ok(self.session_count)
    }

    /// Updates user data file to indicate that user has used the Wizard.
    pub(crate) async fn update_is_returning_wizard(&mut self) -> io::Result<()> {
        *self = update_at_path(DATA_STORE_PATH.as_path(), |data: &mut Self| {
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
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;
    use tokio::fs;

    use super::*;

    #[tokio::test]
    async fn user_data_document_contains_only_internal_data() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("data.json");

        let data = UserData::from_path(&path).await.unwrap();
        let stored: serde_json::Value =
            serde_json::from_slice(&fs::read(path).await.unwrap()).unwrap();

        assert_eq!(
            stored
                .get("session_count")
                .and_then(serde_json::Value::as_u64),
            Some(0)
        );
        assert_eq!(
            stored.get("machine_id").and_then(serde_json::Value::as_str),
            Some(data.machine_id.to_string().as_str())
        );
        assert_eq!(
            stored
                .get("is_returning_wizard")
                .and_then(serde_json::Value::as_bool),
            Some(false)
        );
        assert!(stored.get("user_config").is_none());
    }
}
