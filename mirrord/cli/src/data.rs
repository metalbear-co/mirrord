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

pub(crate) use global_config::{GlobalConfig, GlobalConfigError, global_config_command};
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
pub(crate) async fn update_at_path<T, E>(
    path: &Path,
    update: impl FnOnce(&mut T) -> Result<(), E> + Send + 'static,
) -> Result<T, E>
where
    T: Default + DeserializeOwned + Serialize + Send + 'static,
    E: From<io::Error> + Send + 'static,
{
    update_at_path_inner(path, update, true).await
}

/// Loads and atomically updates a JSON document, failing if the existing contents cannot be
/// represented by `T`.
pub(crate) async fn update_at_path_strict<T, E>(
    path: &Path,
    update: impl FnOnce(&mut T) -> Result<(), E> + Send + 'static,
) -> Result<T, E>
where
    T: Default + DeserializeOwned + Serialize + Send + 'static,
    E: From<io::Error> + Send + 'static,
{
    update_at_path_inner(path, update, false).await
}

fn create_parent_for_missing_target(path: &Path) -> io::Result<()> {
    match fs::metadata(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                fs::create_dir_all(parent)?;
            }
            Ok(())
        }
        Err(error) => Err(error),
    }
}

fn canonical_json<T: Serialize + ?Sized>(value: &T) -> io::Result<Vec<u8>> {
    let mut contents = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    contents.push(b'\n');
    Ok(contents)
}

async fn update_at_path_inner<T, E>(
    path: &Path,
    update: impl FnOnce(&mut T) -> Result<(), E> + Send + 'static,
    recover_invalid: bool,
) -> Result<T, E>
where
    T: Default + DeserializeOwned + Serialize + Send + 'static,
    E: From<io::Error> + Send + 'static,
{
    let path = path.to_owned();
    task::spawn_blocking(move || {
        create_parent_for_missing_target(&path).map_err(E::from)?;

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
            Err(error) => return Err(E::from(error)),
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
                return Err(E::from(io::Error::new(io::ErrorKind::InvalidData, error)));
            }
            None => T::default(),
        };

        update(&mut data)?;

        let contents = canonical_json(&data).map_err(E::from)?;
        if previous.as_deref() != Some(contents.as_slice()) {
            let mut store_file = AtomicWriteFile::open(&path).map_err(E::from)?;
            store_file.write_all(&contents).map_err(E::from)?;
            store_file.commit().map_err(E::from)?;
        }

        Ok(data)
    })
    .await
    .map_err(io::Error::other)
    .map_err(E::from)?
}

/// Creates a missing JSON document as an empty object without parsing or rewriting existing data.
pub(crate) async fn initialize_empty_json_at_path(path: &Path) -> io::Result<()> {
    let path = path.to_owned();
    task::spawn_blocking(move || {
        create_parent_for_missing_target(&path)?;

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
                let contents = canonical_json(&serde_json::json!({}))?;
                let mut store_file = AtomicWriteFile::open(&path)?;
                store_file.write_all(&contents)?;
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
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    use std::time::Duration;

    use serde::{Deserialize, Serialize};
    use tempfile::tempdir;
    use thiserror::Error;
    use tokio::{fs, join, time::sleep};

    use super::*;

    #[derive(Debug, Default, Deserialize, Serialize)]
    struct TestData {
        #[serde(default)]
        count: u32,
        #[serde(default)]
        enabled: bool,
    }

    #[derive(Debug, Error)]
    enum TestUpdateError {
        #[error(transparent)]
        Io(#[from] io::Error),

        #[error("rejected update")]
        Rejected,
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn loading_canonical_data_does_not_rewrite_it() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("test.json");
        let original = canonical_json(&TestData {
            count: 7,
            enabled: true,
        })
        .unwrap();
        fs::write(&path, &original).await.unwrap();
        let original_inode = fs::metadata(&path).await.unwrap().ino();

        let data: TestData = update_at_path(&path, |_| Ok::<_, io::Error>(()))
            .await
            .unwrap();

        assert_eq!(data.count, 7);
        assert_eq!(fs::read(&path).await.unwrap(), original);
        assert_eq!(fs::metadata(path).await.unwrap().ino(), original_inode);
    }

    #[tokio::test]
    async fn loading_valid_legacy_data_adds_missing_fields() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("test.json");
        fs::write(&path, br#"{"count":7}"#).await.unwrap();

        let data: TestData = update_at_path(&path, |_| Ok::<_, io::Error>(()))
            .await
            .unwrap();

        assert_eq!(
            fs::read(path).await.unwrap(),
            canonical_json(&data).unwrap()
        );
    }

    #[tokio::test]
    async fn loading_malformed_data_replaces_it_with_defaults() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("test.json");
        fs::write(&path, br#"{"count": "unfinished""#)
            .await
            .unwrap();

        let data: TestData = update_at_path(&path, |_| Ok::<_, io::Error>(()))
            .await
            .unwrap();

        assert_eq!(data.count, 0);
        assert_eq!(
            fs::read(path).await.unwrap(),
            canonical_json(&data).unwrap()
        );
    }

    #[tokio::test]
    async fn rejected_update_preserves_existing_data() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("test.json");
        let original = br#"{ "count": 7 }"#;
        fs::write(&path, original).await.unwrap();

        let result =
            update_at_path(&path, |_data: &mut TestData| Err(TestUpdateError::Rejected)).await;

        assert!(matches!(result, Err(TestUpdateError::Rejected)));
        assert_eq!(fs::read(path).await.unwrap(), original);
    }

    #[tokio::test]
    async fn strict_update_preserves_malformed_data() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("test.json");
        let original = br#"{"count": "unfinished""#;
        fs::write(&path, original).await.unwrap();

        let result = update_at_path_strict::<TestData, io::Error>(&path, |_| Ok(())).await;

        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(path).await.unwrap(), original);
    }

    #[tokio::test]
    async fn update_rereads_a_concurrently_created_file_under_lock() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("test.json");
        let lock_file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("lock"))
            .unwrap();
        lock_file.lock_exclusive().unwrap();

        let update_path = path.clone();
        let update = tokio::spawn(async move {
            update_at_path(&update_path, |data: &mut TestData| {
                data.count += 1;
                Ok::<_, io::Error>(())
            })
            .await
        });
        sleep(Duration::from_millis(50)).await;
        fs::write(&path, br#"{"count":41,"enabled":false}"#)
            .await
            .unwrap();
        FileExt::unlock(&lock_file).unwrap();

        let data = update.await.unwrap().unwrap();

        assert_eq!(data.count, 42);
    }

    #[tokio::test]
    async fn concurrent_updates_preserve_both_changes() {
        let directory = tempdir().unwrap();
        let path = directory.path().join("test.json");

        let first = update_at_path(&path, |data: &mut TestData| {
            data.count += 1;
            Ok::<_, io::Error>(())
        });
        let second = update_at_path(&path, |data: &mut TestData| {
            data.count += 1;
            Ok::<_, io::Error>(())
        });
        let (first, second) = join!(first, second);
        first.unwrap();
        second.unwrap();

        let data: TestData = update_at_path(&path, |_| Ok::<_, io::Error>(()))
            .await
            .unwrap();
        assert_eq!(data.count, 2);
    }
}
