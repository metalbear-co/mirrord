use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
};

use const_random::const_random;
use mirrord_progress::Progress;
use tracing::debug;

use crate::{error::CliError, Result};

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
) -> Result<PathBuf>
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
            .map_err(|e| CliError::LayerExtractFailed(file_path.clone(), e))?;
        let bytes = include_bytes!(env!("MIRRORD_LAYER_FILE"));
        file.write_all(bytes).unwrap();
        debug!("Extracted library file to {:?}", &file_path);
    }

    progress.success(Some("layer extracted"));
    Ok(file_path)
}
