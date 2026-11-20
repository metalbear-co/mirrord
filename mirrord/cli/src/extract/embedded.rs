//! The layer that the CLI embeds, which it writes to a file so that programs can load it.

use std::path::PathBuf;

use mirrord_progress::Progress;

use super::{default_layer_dir, write_layer};
use crate::CliResult;

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

const LAYER: &[u8] = include_bytes!(env!("MIRRORD_BUNDLED_LAYER"));

/// The arm64 layer, which the arm64e shim in the universal [`LAYER`] loads.
#[cfg(target_os = "macos")]
const LAYER_ARM64: &[u8] = include_bytes!(env!("MIRRORD_BUNDLED_LAYER_MACOS_ARM64"));

/// Names the files that the embedded layers are written to, so that a CLI never loads a layer that
/// another build wrote. It's derived from a digest of the layers (see `scripts/layer_id.py`), so
/// two builds of the same layers write the same files.
const LAYER_ID: &str = env!("MIRRORD_BUNDLED_LAYER_ID");

/// Writes the layer to `dest_dir`, or to the temporary directory by default, and returns its path.
/// With `prefix`, the file name starts with [`LAYER_ID`], so that each build of the layer gets a
/// file of its own.
pub(crate) fn extract_library<P>(
    dest_dir: Option<String>,
    progress: &P,
    prefix: bool,
) -> CliResult<PathBuf>
where
    P: Progress,
{
    // Cargo builds embed an empty placeholder for the layer, which `.cargo/config.toml` sets to
    // keep rust-analyzer's `cargo check` working, so this assertion rejects them. Because the
    // function is generic, rustc evaluates the assertion when it generates the CLI's code, which
    // `cargo check` doesn't do.
    const {
        assert!(
            !LAYER.is_empty(),
            "the layer is empty, so build the CLI with `bazel build //mirrord/cli:cli_standalone`"
        )
    };

    let mut progress = progress.subtask("extracting layer");
    let extension = std::env::consts::DLL_EXTENSION;

    let file_name = if prefix {
        format!("{LAYER_ID}-libmirrord_layer.{extension}")
    } else {
        format!("libmirrord_layer.{extension}")
    };

    let file_path = match dest_dir {
        Some(dest_dir) => std::path::Path::new(&dest_dir).join(file_name),
        None => default_layer_dir(&temp_dir(), LAYER_ID, &progress)?.join(file_name),
    };
    write_layer(&file_path, LAYER)?;

    progress.success(Some("layer extracted"));
    Ok(file_path)
}

/// Writes the arm64 layer to the temporary directory and returns its path. `prefix` works as in
/// [`extract_library`].
///
/// An x86-64 CLI writes it too, because it may run under emulation on an arm64 Mac and start
/// arm64e programs.
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
