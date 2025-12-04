use std::{env, ffi::OsString, path::PathBuf};

use dll_syringe::{Syringe, process::OwnedProcess};
use mirrord_progress::Progress;

use crate::{CliError, CliResult, extract::extract_library};

pub(crate) fn attach_command<P>(pid: u32, progress: &P) -> CliResult<()>
where
    P: Progress,
{
    // Extract Layer from exe, or use existing file if MIRRORD_LAYER_FILE env var is set
    // (for debugging)
    let lib_path = match env::var("MIRRORD_LAYER_FILE") {
        Ok(existing_path) => {
            tracing::debug!(
                "Using existing library file from MIRRORD_LAYER_FILE: {}",
                existing_path
            );
            PathBuf::from(existing_path)
        }
        Err(_) => {
            tracing::debug!("MIRRORD_LAYER_FILE not set, extracting library from binary");
            extract_library(None, progress, true)?
        }
    };
    // Set `MIRRORD_LAYER_FILE` for the case it was not already present, as subprocess
    // support will require it.
    unsafe { env::set_var("MIRRORD_LAYER_FILE", &lib_path) };

    let syringe =
        Syringe::for_process(OwnedProcess::from_pid(pid).map_err(CliError::FailedProcessHandle)?);
    progress.info("obtained handle to process");

    let payload_path = OsString::from(lib_path);
    syringe
        .inject(payload_path)
        .map_err(|e| CliError::FailedAttachingLayer(pid, e))?;
    progress.info("succesfully injected");

    Ok(())
}
