//! The layer of a build without `standalone`, which is already a file in its runfiles, at the
//! runfiles path that `MIRRORD_BUNDLED_LAYER` gives it.

use std::path::{Path, PathBuf};

use mirrord_progress::Progress;

use super::write_layer;
use crate::{CliResult, error::CliError};

/// Returns the layer's file in the runfiles, or copies it to `dest_dir`. `prefix` has no effect,
/// because the file in the runfiles is always the layer that this CLI was built with.
pub(crate) fn extract_library<P>(
    dest_dir: Option<String>,
    _progress: &P,
    _prefix: bool,
) -> CliResult<PathBuf>
where
    P: Progress,
{
    let layer = mirrord_runfiles::rlocation!(env!("MIRRORD_BUNDLED_LAYER"));
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

#[cfg(target_os = "macos")]
pub(crate) fn extract_arm64<P>(_progress: &P, _prefix: bool) -> CliResult<PathBuf>
where
    P: Progress,
{
    Ok(mirrord_runfiles::rlocation!(env!(
        "MIRRORD_BUNDLED_LAYER_MACOS_ARM64"
    )))
}
