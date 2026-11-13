use std::path::{Path, PathBuf};

use const_random::const_random;
use mirrord_progress::Progress;
use tracing::debug;

use crate::{CliResult, error::CliError, execution::MIRRORD_LAYER_FILE_ENV};

/// Returns the layer that the CLI loads into the programs that it runs: the file at
/// `MIRRORD_LAYER_FILE`, which developers set to try a layer that they built, or the CLI's own.
pub(crate) fn layer_file<P: Progress>(progress: &P) -> CliResult<PathBuf> {
    match std::env::var_os(MIRRORD_LAYER_FILE_ENV) {
        Some(path) => {
            debug!("Using the layer from {MIRRORD_LAYER_FILE_ENV}: {path:?}");
            Ok(PathBuf::from(path))
        }
        None => {
            debug!("{MIRRORD_LAYER_FILE_ENV} isn't set, so using the CLI's own layer");
            extract_library(None, progress, true)
        }
    }
}

/// For some reason loading dylib from $TMPDIR can get the process killed somehow..?
#[cfg(target_os = "macos")]
mod mac {
    use std::str::FromStr;

    use super::*;

    pub fn temp_dir() -> PathBuf {
        PathBuf::from_str("/tmp/").unwrap()
    }
}

#[cfg(not(target_os = "macos"))]
use std::env::temp_dir;

#[cfg(target_os = "macos")]
use mac::temp_dir;

fn default_layer_dir<P: Progress>(temp_dir: &Path, progress: &P) -> CliResult<PathBuf> {
    let dir = temp_dir.join("mirrord");
    match std::fs::create_dir_all(&dir) {
        Ok(()) => Ok(dir),
        Err(_) if dir.is_file() => {
            // Reuse one directory per build while keeping it available to injected descendants.
            let fallback = temp_dir.join(format!("mirrord-{}", const_random!(u64)));
            std::fs::create_dir_all(&fallback)
                .map_err(|e| CliError::LayerExtractError(fallback.clone(), e))?;
            progress.warning(&format!(
                "{} is a file; extracting the layer to {} instead",
                dir.display(),
                fallback.display()
            ));
            Ok(fallback)
        }
        Err(error) => Err(CliError::LayerExtractError(dir, error)),
    }
}

/// Extract to given directory, or tmp by default.
/// If prefix is true, add a random prefix to the file name that identifies the specific build
/// of the layer. This is useful for debug purposes usually.
pub(crate) fn extract_library<P>(
    dest_dir: Option<String>,
    progress: &P,
    prefix: bool,
) -> CliResult<PathBuf>
where
    P: Progress,
{
    let mut progress = progress.subtask("extracting layer");
    let extension = std::env::consts::DLL_EXTENSION;

    let file_name = if prefix {
        format!("{}-libmirrord_layer.{extension}", const_random!(u64))
    } else {
        format!("libmirrord_layer.{extension}")
    };

    let file_path = match dest_dir {
        Some(dest_dir) => std::path::Path::new(&dest_dir).join(file_name),
        None => default_layer_dir(&temp_dir(), &progress)?.join(file_name),
    };
    write_layer(&file_path, include_bytes!(env!("MIRRORD_BUNDLED_LAYER")))?;

    progress.success(Some("layer extracted"));
    Ok(file_path)
}

/// Extract the arm64 compiled layer for the shim to use (MacOS only).
/// This is done even if on x86 due to the possibility of mirrord being run emulated
/// If prefix is true, add a random prefix to the file name that identifies the specific build
/// of the layer. This is useful for debug purposes usually.
#[cfg(target_os = "macos")]
pub(crate) fn extract_arm64<P>(progress: &P, prefix: bool) -> CliResult<PathBuf>
where
    P: Progress,
{
    let mut progress = progress.subtask("extracting arm64 layer library");
    let extension = std::env::consts::DLL_EXTENSION;

    let file_name = if prefix {
        format!("{}-libmirrord_layer_arm64.{extension}", const_random!(u64))
    } else {
        format!("libmirrord_layer_arm64.{extension}")
    };

    let file_path = temp_dir().as_path().join(file_name);
    write_layer(
        &file_path,
        include_bytes!(env!("MIRRORD_BUNDLED_LAYER_MACOS_ARM64")),
    )?;

    progress.success(Some("arm64 layer library extracted"));
    Ok(file_path)
}

/// Writes `bytes` to `path`, unless the file is already there.
///
/// The bytes go to a temporary file next to `path` first, which then takes `path`'s name, so that
/// another CLI, which can start at the same time, never sees a half-written layer. When another CLI
/// wrote the file first, its file stays, because a program may already have loaded it.
fn write_layer(path: &Path, bytes: &[u8]) -> CliResult<()> {
    if bytes.is_empty() {
        return Err(CliError::EmptyBundledLayer);
    }
    if path.exists() {
        return Ok(());
    }

    let error = |error| CliError::LayerExtractError(path.to_owned(), error);
    let directory = path.parent().unwrap_or(Path::new("."));
    let file = tempfile::NamedTempFile::new_in(directory).map_err(error)?;
    std::fs::write(file.path(), bytes).map_err(error)?;

    // Readable by every user, whatever the umask, because a program that mirrord runs may start a
    // child as another user, and that child loads the layer too.
    #[cfg(unix)]
    file.as_file()
        .set_permissions(std::os::unix::fs::PermissionsExt::from_mode(0o644))
        .map_err(error)?;

    match file.persist_noclobber(path) {
        Err(persist) if persist.error.kind() != std::io::ErrorKind::AlreadyExists => {
            return Err(error(persist.error));
        }
        _ => {}
    }

    debug!("Extracted layer to {path:?}");
    Ok(())
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::{fs, sync::Mutex};

    use super::*;

    #[derive(Default)]
    struct RecordingProgress(Mutex<Vec<String>>);

    impl Progress for RecordingProgress {
        fn subtask(&self, _: &str) -> Self {
            Self::default()
        }

        fn warning(&self, message: &str) {
            self.0.lock().unwrap().push(message.to_owned());
        }
    }

    #[test]
    fn layer_directory_falls_back_when_path_is_a_file() {
        let root = tempfile::tempdir().unwrap();
        let occupied = root.path().join("mirrord");
        fs::write(&occupied, b"existing file").unwrap();
        let progress = RecordingProgress::default();

        let directory = default_layer_dir(root.path(), &progress).unwrap();
        assert_eq!(directory.parent(), Some(root.path()));
        assert_ne!(directory, occupied);
        fs::write(directory.join("layer"), b"layer contents").unwrap();
        assert_eq!(fs::read(&occupied).unwrap(), b"existing file");
        let warnings = progress.0.lock().unwrap();
        assert_eq!(warnings.len(), 1);
        let warning = warnings.first().unwrap();
        assert!(warning.contains(&occupied.display().to_string()));
        assert!(warning.contains(&directory.display().to_string()));
    }

    #[test]
    fn fallback_directory_is_reused_across_invocations() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("mirrord"), b"existing file").unwrap();
        let directory = default_layer_dir(root.path(), &RecordingProgress::default()).unwrap();
        fs::write(directory.join("layer"), b"cached layer").unwrap();

        for _ in 0..3 {
            assert_eq!(
                default_layer_dir(root.path(), &RecordingProgress::default()).unwrap(),
                directory
            );
        }

        assert_eq!(fs::read(directory.join("layer")).unwrap(), b"cached layer");
        assert_eq!(fs::read_dir(root.path()).unwrap().count(), 2);
    }

    #[test]
    fn layer_directory_is_created_and_reused_without_warning() {
        let root = tempfile::tempdir().unwrap();
        let progress = RecordingProgress::default();
        let directory = default_layer_dir(root.path(), &progress).unwrap();
        assert_eq!(directory, root.path().join("mirrord"));
        fs::write(directory.join("layer"), b"cached layer").unwrap();

        assert_eq!(
            default_layer_dir(root.path(), &progress).unwrap(),
            directory
        );
        assert_eq!(fs::read(directory.join("layer")).unwrap(), b"cached layer");
        assert!(progress.0.lock().unwrap().is_empty());
    }

    /// A program that mirrord runs may start a child as another user, which must be able to load
    /// the layer too.
    #[cfg(unix)]
    #[test]
    fn extracted_layer_is_readable_by_every_user() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("layer");
        write_layer(&path, b"layer contents").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644, "the layer's mode is {mode:o}");
    }

    /// `mirrord extract` can run again with the same directory.
    #[test]
    fn writing_the_layer_again_keeps_the_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("layer");
        write_layer(&path, b"layer contents").unwrap();
        write_layer(&path, b"other contents").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"layer contents");
    }
}
