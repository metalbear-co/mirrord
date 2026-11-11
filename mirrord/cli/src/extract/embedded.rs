//! The layer that the CLI embeds, which it writes to a file to load it.

use std::path::{Path, PathBuf};

use const_random::const_random;
use mirrord_progress::Progress;

use super::write_layer;
use crate::{CliResult, error::CliError};

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

/// The layer that the CLI loads into the programs that it runs.
const LAYER: &[u8] = include_bytes!(env!("MIRRORD_BUNDLED_LAYER"));

/// The arm64 layer, which the arm64e shim of the universal [`LAYER`] loads.
#[cfg(target_os = "macos")]
const LAYER_ARM64: &[u8] = include_bytes!(env!("MIRRORD_BUNDLED_LAYER_MACOS_ARM64"));

/// Identifies this build of the embedded layers, and names the files that they're extracted to, so
/// that each build gets files of its own rather than reusing the files of another build. The CLI
/// compiles again whenever an embedded layer changes, so a number that's random at compile time
/// differs between any two builds of the layers.
const LAYER_ID: u64 = const_random!(u64);

fn default_layer_dir<P: Progress>(temp_dir: &Path, progress: &P) -> CliResult<PathBuf> {
    let dir = temp_dir.join("mirrord");
    match std::fs::create_dir_all(&dir) {
        Ok(()) => Ok(dir),
        Err(_) if dir.is_file() => {
            // Reuse one directory per build while keeping it available to injected descendants.
            let fallback = temp_dir.join(format!("mirrord-{LAYER_ID}"));
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
/// If prefix is true, add [`LAYER_ID`] as a prefix to the file name, which identifies the
/// specific build of the layer. This is useful for debug purposes usually.
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
        format!("{LAYER_ID}-libmirrord_layer.{extension}")
    } else {
        format!("libmirrord_layer.{extension}")
    };

    let file_path = match dest_dir {
        Some(dest_dir) => std::path::Path::new(&dest_dir).join(file_name),
        None => default_layer_dir(&temp_dir(), &progress)?.join(file_name),
    };
    write_layer(&file_path, LAYER)?;

    progress.success(Some("layer extracted"));
    Ok(file_path)
}

/// Extract the arm64 compiled layer for the shim to use (MacOS only).
/// This is done even if on x86 due to the possibility of mirrord being run emulated
/// If prefix is true, add [`LAYER_ID`] as a prefix to the file name, which identifies the
/// specific build of the layer. This is useful for debug purposes usually.
#[cfg(target_os = "macos")]
pub(crate) fn extract_arm64<P>(progress: &P, prefix: bool) -> CliResult<PathBuf>
where
    P: Progress,
{
    let mut progress = progress.subtask("extracting arm64 layer library");
    let extension = std::env::consts::DLL_EXTENSION;

    let file_name = if prefix {
        format!("{LAYER_ID}-libmirrord_layer_arm64.{extension}")
    } else {
        format!("libmirrord_layer_arm64.{extension}")
    };

    let file_path = temp_dir().as_path().join(file_name);
    write_layer(&file_path, LAYER_ARM64)?;

    progress.success(Some("arm64 layer library extracted"));
    Ok(file_path)
}

#[cfg(test)]
mod tests {
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
}
