use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};

use const_random::const_random;
use mirrord_progress::Progress;
use tracing::debug;

use crate::{error::CliError, CliResult};

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

/// Extract to given directory, or tmp by default.
/// If prefix is true, add a random prefix to the file name that identifies the specific build
/// of the layer. This is useful for debug purposes usually.
pub(crate) fn extract_library<P>(
    dest_dir: Option<String>,
    progress: &P,
    prefix: bool,
) -> CliResult<PathBuf>
where
    P: Progress + Send + Sync,
{
    let mut progress = progress.subtask("extracting layer");
    let extension = Path::new(env!("MIRRORD_LAYER_FILE"))
        .extension()
        .unwrap()
        .to_str()
        .unwrap();

    let file_name = if prefix {
        format!("{}-libmirrord_layer.{extension}", const_random!(u64))
    } else {
        format!("libmirrord_layer.{extension}")
    };

    let file_path = match dest_dir {
        Some(dest_dir) => std::path::Path::new(&dest_dir).join(file_name),
        None => temp_dir().as_path().join(file_name),
    };
    if !file_path.exists() {
        let mut file = File::create(&file_path)
            .map_err(|e| CliError::LayerExtractError(file_path.clone(), e))?;
        let bytes = include_bytes!(env!("MIRRORD_LAYER_FILE"));
        file.write_all(bytes).unwrap();
        debug!("Extracted library file to {:?}", &file_path);
    }

    progress.success(Some("layer extracted"));
    Ok(file_path)
}

/// Extract the arm64 compiled layer for the shim to use (MacOS only).
/// This is done even if on x86 due to the possibility of mirrord being run emulated
/// If prefix is true, add a random prefix to the file name that identifies the specific build
/// of the layer. This is useful for debug purposes usually.
#[cfg(all(target_os = "macos"))]
pub(crate) fn extract_arm64<P>(progress: &P, prefix: bool) -> CliResult<PathBuf>
where
    P: Progress + Send + Sync,
{
    let mut progress = progress.subtask("extracting arm64 layer library");
    let extension = Path::new(env!("MIRRORD_LAYER_FILE_MACOS_ARM64"))
        .extension()
        .unwrap()
        .to_str()
        .unwrap();

    let file_name = if prefix {
        format!("{}-libmirrord_layer_arm64.{extension}", const_random!(u64))
    } else {
        format!("libmirrord_layer_arm64.{extension}")
    };

    let file_path = temp_dir().as_path().join(file_name);
    if !file_path.exists() {
        let mut file = File::create(&file_path)
            .map_err(|e| CliError::LayerExtractError(file_path.clone(), e))?;
        let bytes = include_bytes!(env!("MIRRORD_LAYER_FILE_MACOS_ARM64"));
        file.write_all(bytes).unwrap();
        debug!("Extracted arm64 layer library to {:?}", &file_path);
    }

    progress.success(Some("arm64 layer library extracted"));
    Ok(file_path)
}
