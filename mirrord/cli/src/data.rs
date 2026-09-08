use std::{
    any::type_name,
    env::home_dir,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};

use atomic_write_file::AtomicWriteFile;
use fs4::fs_std::FileExt;
use serde::{Serialize, de::DeserializeOwned};
use tokio::task;
use tracing::trace;

mod global_config;
mod user_data;

pub(crate) use global_config::GlobalConfig;
pub(crate) use user_data::UserData;

/// Returns the path to a document stored in mirrord's user-wide data directory.
pub(crate) fn default_path(file_name: &str) -> PathBuf {
    home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".mirrord")
        .join(file_name)
}

/// Loads and atomically updates a mirrord-owned JSON document.
///
/// Atomic replacement changes the document's inode, so writers coordinate through a sidecar
/// lock whose identity remains stable across commits.
pub(crate) async fn update_at_path<T>(
    path: &Path,
    update: impl FnOnce(&mut T) + Send + 'static,
) -> io::Result<T>
where
    T: Default + DeserializeOwned + Serialize + Send + 'static,
{
    update_at_path_inner(path, update, true).await
}

/// Loads and atomically updates a JSON document, failing if the existing contents cannot be
/// represented by `T`.
pub(crate) async fn update_at_path_strict<T>(
    path: &Path,
    update: impl FnOnce(&mut T) + Send + 'static,
) -> io::Result<T>
where
    T: Default + DeserializeOwned + Serialize + Send + 'static,
{
    update_at_path_inner(path, update, false).await
}

async fn update_at_path_inner<T>(
    path: &Path,
    update: impl FnOnce(&mut T) + Send + 'static,
    recover_invalid: bool,
) -> io::Result<T>
where
    T: Default + DeserializeOwned + Serialize + Send + 'static,
{
    let path = path.to_owned();
    task::spawn_blocking(move || {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

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
        let mut data = match previous.as_deref().map(serde_json::from_slice) {
            Some(Ok(data)) => data,
            Some(Err(error)) if recover_invalid => {
                trace!(
                    %error,
                    data_type = type_name::<T>(),
                    "Could not deserialize mirrord data; replacing it with defaults"
                );
                T::default()
            }
            Some(Err(error)) => {
                return Err(io::Error::new(io::ErrorKind::InvalidData, error));
            }
            None => T::default(),
        };

        update(&mut data);

        let contents = serde_json::to_vec(&data).map_err(io::Error::other)?;
        if previous.as_deref() != Some(contents.as_slice()) {
            let mut store_file = AtomicWriteFile::open(&path)?;
            store_file.write_all(&contents)?;
            store_file.commit()?;
        }

        Ok(data)
    })
    .await
    .map_err(io::Error::other)?
}

/// Creates a missing JSON document as an empty object without parsing or rewriting existing data.
pub(crate) async fn initialize_empty_json_at_path(path: &Path) -> io::Result<()> {
    let path = path.to_owned();
    task::spawn_blocking(move || {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let lock_path = path.with_extension("lock");
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(lock_path)?;
        lock_file.lock_exclusive()?;

        match fs::metadata(&path) {
            Ok(_) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let mut store_file = AtomicWriteFile::open(&path)?;
                store_file.write_all(b"{}")?;
                store_file.commit()
            }
            Err(error) => Err(error),
        }
    })
    .await
    .map_err(io::Error::other)?
}

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};
    use tempfile::tempdir;
    use tokio::{fs, join};

    use super::*;

    #[derive(Debug, Default, Deserialize, Serialize)]
    struct TestData {
        #[serde(default)]
        count: u32,
        #[serde(default)]
        enabled: bool,
    }

    #[tokio::test]
    async fn loading_existing_data_does_not_rewrite_it() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("test.json");
        let original = serde_json::to_vec(&TestData {
            count: 7,
            enabled: true,
        })
        .unwrap();
        fs::write(&path, &original).await.unwrap();

        let data: TestData = update_at_path(&path, |_| {}).await.unwrap();

        assert_eq!(data.count, 7);
        assert_eq!(fs::read(path).await.unwrap(), original);
    }

    #[tokio::test]
    async fn loading_valid_legacy_data_adds_missing_fields() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("test.json");
        fs::write(&path, br#"{"count":7}"#).await.unwrap();

        let data: TestData = update_at_path(&path, |_| {}).await.unwrap();

        assert_eq!(
            fs::read(path).await.unwrap(),
            serde_json::to_vec(&data).unwrap()
        );
    }

    #[tokio::test]
    async fn loading_malformed_data_replaces_it_with_defaults() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("test.json");
        fs::write(&path, br#"{"count": "unfinished""#)
            .await
            .unwrap();

        let data: TestData = update_at_path(&path, |_| {}).await.unwrap();

        assert_eq!(data.count, 0);
        assert_eq!(
            fs::read(path).await.unwrap(),
            serde_json::to_vec(&data).unwrap()
        );
    }

    #[tokio::test]
    async fn concurrent_updates_preserve_both_changes() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("test.json");

        let first = update_at_path(&path, |data: &mut TestData| data.count += 1);
        let second = update_at_path(&path, |data: &mut TestData| data.count += 1);
        let (first, second) = join!(first, second);
        first.unwrap();
        second.unwrap();

        let data: TestData = update_at_path(&path, |_| {}).await.unwrap();
        assert_eq!(data.count, 2);
    }
}
