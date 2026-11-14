//! Gives the CLI the layer that it loads into the programs that it runs, as a file.
//!
//! The CLI embeds the layer and writes it to a file (`extract/embedded.rs`), unless it's built
//! with `bazel-runfiles`, which finds the layer as a file in Bazel's runfiles instead
//! (`extract/runfiles.rs`, and see `bundle.rs`).

use std::path::{Path, PathBuf};

use mirrord_progress::Progress;
use tracing::debug;

use crate::{CliResult, error::CliError, execution::MIRRORD_LAYER_FILE_ENV};

#[cfg_attr(not(feature = "bazel-runfiles"), path = "extract/embedded.rs")]
#[cfg_attr(feature = "bazel-runfiles", path = "extract/runfiles.rs")]
mod bundled;

#[cfg(target_os = "macos")]
pub(crate) use bundled::extract_arm64;
pub(crate) use bundled::extract_library;

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

    use super::*;

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
