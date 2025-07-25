use std::{path::PathBuf, sync::LazyLock};

use serde::{Deserialize, Serialize};
use tokio::{
    fs,
    io::{self, AsyncReadExt, AsyncWriteExt},
};
use tracing::trace;
use uuid::Uuid;

/// "~/.mirrord"
static DATA_STORE_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    home::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".mirrord")
});

/// "~/.mirrord/data.json"
static DATA_STORE_PATH: LazyLock<PathBuf> = LazyLock::new(|| DATA_STORE_DIR.join("data.json"));

/// Data that we store in the user's machine at `~/.mirrord/data.json` that might be used
/// for a variety of purposes.
///
/// Whenever we deserialize the `UserData` json file, if there are any errors we generate
/// a new one using [`UserData::default`]. To avoid overwriting the file with all new default
/// values in case we got a deserialization error due to a missing field, each field here
/// gets a `default` annotation, so only the missing fields will be updated.
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct UserData {
    /// Amount of times this user has run mirrord.
    #[serde(default)]
    session_count: u32,

    /// Helps us keep track of unique users for analytics when telemetry is enabled.
    ///
    /// Must use custom `default =`, since the default is [`Uuid::nil`].
    #[serde(default = "default_uuid")]
    machine_id: Uuid,
}

/// When deserialziing a [`UserData`] file, the `machine_id` might not be present, but
/// we don't want `serde` to error and overwrite the other [`UserData`] fields with
/// default values.
fn default_uuid() -> Uuid {
    Uuid::new_v4()
}

impl Default for UserData {
    fn default() -> Self {
        Self {
            session_count: 0,
            machine_id: Uuid::new_v4(),
        }
    }
}

impl UserData {
    /// Create `UserData` from the default file path (`DATA_STORE_PATH`)
    pub(crate) async fn from_default_path() -> io::Result<Self> {
        let read_from_file = async || {
            if !DATA_STORE_DIR.exists() {
                fs::create_dir_all(&*DATA_STORE_DIR).await?;
            }

            let mut store_file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(DATA_STORE_PATH.as_path())
                .await?;

            let mut contents = vec![];
            store_file.read_to_end(&mut contents).await?;
            let user_data: UserData = serde_json::from_slice(contents.as_slice())?;

            Ok::<_, io::Error>(user_data)
        };

        match read_from_file().await {
            Ok(user_data) => {
                // Forwards compat note:
                //
                // Always update the file to fill it with potentially new fields that might
                // be missing from the user's store.
                user_data
                    .overwrite_to_file()
                    .await
                    .inspect_err(|fail| trace!(%fail, "Updating `UserData` file failed!"))?;

                Ok(user_data)
            }
            Err(fail) => {
                trace!(
                    %fail,
                    "Could not load `UserData` from file! Attempting to create it..."
                );

                let user_data = Self::default();
                user_data.overwrite_to_file().await.inspect_err(|fail| {
                    trace!(%fail, "Creating a default `UserData` file failed!\
                        There are no guarantees that the `UserData` will be stored anywhere.")
                })?;
                Ok(user_data)
            }
        }
    }

    /// Overwrite the JSON contents at the default file path (`DATA_STORE_PATH`) with `UserData`
    pub(crate) async fn overwrite_to_file(&self) -> Result<(), io::Error> {
        // DATA_STORE_DIR and DATA_STORE_PATH are already known to exist
        let mut store_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(DATA_STORE_PATH.as_path())
            .await?;

        let contents = serde_json::to_vec(&self)?;
        store_file.write_all(contents.as_slice()).await?;
        Ok(())
    }

    /// Increases the session count by one and returns the number.
    pub(crate) async fn bump_session_count(&mut self) -> io::Result<u32> {
        self.session_count += 1;

        self.overwrite_to_file().await?;
        Ok(self.session_count)
    }

    pub(crate) fn machine_id(&self) -> Uuid {
        self.machine_id
    }
}
