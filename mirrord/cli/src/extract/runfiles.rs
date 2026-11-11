//! The layer that a `bazel-runfiles` build finds in its runfiles, where it's already a file that
//! programs can load (see `bundle.rs`). Each build of the layer is a file of its own there, so the
//! file name needs no prefix that identifies the build.

use std::path::{Path, PathBuf};

use mirrord_progress::Progress;

use super::write_layer;
use crate::{CliResult, error::CliError};

/// Returns the layer's file in the runfiles, or copies it to `dest_dir` as `libmirrord_layer` when
/// it's given.
pub(crate) fn extract_library<P>(
    dest_dir: Option<String>,
    _progress: &P,
    _prefix: bool,
) -> CliResult<PathBuf>
where
    P: Progress,
{
    let layer = crate::bundle::runfile(env!("MIRRORD_BUNDLED_LAYER"));
    let Some(dest_dir) = dest_dir else {
        return Ok(layer);
    };

    let extension = std::env::consts::DLL_EXTENSION;
    let file_path = Path::new(&dest_dir).join(format!("libmirrord_layer.{extension}"));
    let bytes =
        std::fs::read(&layer).map_err(|error| CliError::LayerExtractError(layer.clone(), error))?;
    write_layer(&file_path, &bytes)?;
    Ok(file_path)
}

/// Returns the arm64 layer's file in the runfiles, which the shim can load as it is.
#[cfg(target_os = "macos")]
pub(crate) fn extract_arm64<P>(_progress: &P, _prefix: bool) -> CliResult<PathBuf>
where
    P: Progress,
{
    Ok(crate::bundle::runfile(env!(
        "MIRRORD_BUNDLED_LAYER_MACOS_ARM64"
    )))
}
