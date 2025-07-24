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

#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct UserData {
    session_count: u32,
    machine_id: Uuid,
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
    pub(crate) async fn from_default_path() -> Result<Self, io::Error> {
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
            Ok(user_data) => Ok(user_data),
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
    /// Accesses the count via a file in the global .mirrord dir
    pub(crate) async fn bump_session_count(&mut self) -> Result<u32, io::Error> {
        self.session_count += 1;

        self.overwrite_to_file().await?;
        Ok(self.session_count)
    }

    pub(crate) fn machine_id(&self) -> Uuid {
        self.machine_id
    }
}
