//! Gives the CLI the layer that it loads into the programs that it runs, as a file.

use std::path::Path;
#[cfg(any(feature = "standalone", test))]
use std::path::PathBuf;

#[cfg(any(feature = "standalone", test))]
use mirrord_progress::Progress;
use tracing::debug;

use crate::{CliResult, error::CliError};

// The CLI uses the layer's file in its runfiles, unless it's a `standalone` build, which embeds
// the layer and writes it to a file.
#[cfg_attr(feature = "standalone", path = "extract/embedded.rs")]
#[cfg_attr(not(feature = "standalone"), path = "extract/runfiles.rs")]
mod bundled;

#[cfg(target_os = "macos")]
pub(crate) use bundled::extract_arm64;
pub(crate) use bundled::extract_library;

/// Returns the directory in `temp_dir` that the layer is written to, `mirrord`, and creates it.
///
/// When `mirrord` is a file, it returns `mirrord-<layer_id>` instead, so each build of the layer
/// has one directory, which every invocation reuses.
///
/// Only the standalone CLI writes the layer to the temporary directory, but the CLI's tests, which
/// build it without `standalone`, test this too.
#[cfg(any(feature = "standalone", test))]
fn default_layer_dir<P: Progress>(
    temp_dir: &Path,
    layer_id: &str,
    progress: &P,
) -> CliResult<PathBuf> {
    let dir = temp_dir.join("mirrord");
    match std::fs::create_dir_all(&dir) {
        Ok(()) => Ok(dir),
        Err(_) if dir.is_file() => {
            // Reuse one directory per build while keeping it available to injected descendants.
            let fallback = temp_dir.join(format!("mirrord-{layer_id}"));
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

/// Writes `bytes` to `path` atomically, so that no program loads a layer that another CLI is still
/// writing.
fn write_layer(path: &Path, bytes: &[u8]) -> CliResult<()> {
    // A program may already have loaded the existing file, so it stays.
    if path.exists() {
        return Ok(());
    }

    let error = |error| CliError::LayerExtractError(path.to_owned(), error);
    let directory = path.parent().unwrap_or(Path::new("."));
    let file = tempfile::NamedTempFile::new_in(directory).map_err(error)?;
    std::fs::write(file.path(), bytes).map_err(error)?;

    // `NamedTempFile` creates the file readable only by its owner, but a program that mirrord runs
    // may start a child as another user, and that child loads the layer too.
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

    const LAYER_ID: &str = "0123456789abcdef";

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

        let directory = default_layer_dir(root.path(), LAYER_ID, &progress).unwrap();
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
        let directory =
            default_layer_dir(root.path(), LAYER_ID, &RecordingProgress::default()).unwrap();
        fs::write(directory.join("layer"), b"cached layer").unwrap();

        for _ in 0..3 {
            assert_eq!(
                default_layer_dir(root.path(), LAYER_ID, &RecordingProgress::default()).unwrap(),
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
        let directory = default_layer_dir(root.path(), LAYER_ID, &progress).unwrap();
        assert_eq!(directory, root.path().join("mirrord"));
        fs::write(directory.join("layer"), b"cached layer").unwrap();

        assert_eq!(
            default_layer_dir(root.path(), LAYER_ID, &progress).unwrap(),
            directory
        );
        assert_eq!(fs::read(directory.join("layer")).unwrap(), b"cached layer");
        assert!(progress.0.lock().unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn extracted_layer_is_readable_by_every_user() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("layer");
        write_layer(&path, b"layer contents").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644, "the layer's mode is {mode:o}");
    }

    #[test]
    fn writing_the_layer_again_keeps_the_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("layer");
        write_layer(&path, b"layer contents").unwrap();
        write_layer(&path, b"other contents").unwrap();

        assert_eq!(std::fs::read(&path).unwrap(), b"layer contents");
    }
}
