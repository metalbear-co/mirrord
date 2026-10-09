use std::{
    collections::{BTreeMap, HashSet},
    env,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
    ptr,
};

use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};

use crate::{SipError, error::Result};

pub const MIRRORD_ROSETTA_FALLBACKS_PATH_ENV: &str = "MIRRORD_ROSETTA_FALLBACKS_PATH";

/// One protected binary that required Rosetta because it was absent from the native bundle.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct RosettaFallbackEntry {
    pub binary_path: String,
    pub os_version: String,
}

#[derive(Deserialize, Serialize)]
struct StoredRosettaFallback {
    binary_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    os_version: Option<String>,
    #[serde(default)]
    reported: bool,
}

pub fn current_macos_version() -> Result<String> {
    let name = c"kern.osproductversion";
    let mut length = 0;
    // SAFETY: the name is static; the first call gets the size (mut length) used for the second
    // call's buffer.
    let mut version = unsafe {
        if libc::sysctlbyname(
            name.as_ptr(),
            ptr::null_mut(),
            &mut length,
            ptr::null_mut(),
            0,
        ) != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }

        let mut version = vec![0_u8; length];
        if libc::sysctlbyname(
            name.as_ptr(),
            version.as_mut_ptr().cast(),
            &mut length,
            ptr::null_mut(),
            0,
        ) != 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        version
    };
    version.truncate(length);

    Ok(std::str::from_utf8(&version)?
        .trim_end_matches('\0')
        .to_owned())
}

pub fn rosetta_fallbacks_path() -> Result<PathBuf> {
    if let Some(path) = env::var_os(MIRRORD_ROSETTA_FALLBACKS_PATH_ENV) {
        return Ok(path.into());
    }

    let home =
        env::var_os("HOME").ok_or_else(|| SipError::UnlikelyError("HOME is not set".to_owned()))?;
    Ok(PathBuf::from(home)
        .join(".mirrord")
        .join("rosetta-fallbacks.jsonl"))
}

fn open_rosetta_fallback_report_lock() -> Result<File> {
    let mut lock_path = rosetta_fallbacks_path()?.into_os_string();
    lock_path.push(".report.lock");
    let lock_path = PathBuf::from(lock_path);
    let parent = lock_path.parent().ok_or_else(|| {
        SipError::UnlikelyError("Rosetta fallback report lock has no parent directory".to_owned())
    })?;
    fs::create_dir_all(parent)?;

    Ok(OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?)
}

/// Gets a lock on the fallback report so that multiple processes don't send the same report.
pub fn acquire_rosetta_fallback_report_lock() -> Result<File> {
    let file = open_rosetta_fallback_report_lock()?;
    file.lock_exclusive()?;
    Ok(file)
}

/// Tries to lock the report without delaying session startup.
pub fn try_acquire_rosetta_fallback_report_lock() -> Result<Option<File>> {
    let file = open_rosetta_fallback_report_lock()?;
    match file.try_lock_exclusive() {
        Ok(true) => Ok(Some(file)),
        Ok(false) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn open_rosetta_fallbacks_for_append() -> Result<(File, Vec<u8>)> {
    let path = rosetta_fallbacks_path()?;
    let parent = path.parent().ok_or_else(|| {
        SipError::UnlikelyError("Rosetta fallback report has no parent directory".to_owned())
    })?;
    fs::create_dir_all(parent)?;

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    file.lock_exclusive()?;

    let mut contents = Vec::new();
    file.read_to_end(&mut contents)?;
    if !contents.is_empty() && !contents.ends_with(b"\n") {
        let valid_length = contents
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(0, |index| index + 1);
        contents.truncate(valid_length);
        file.set_len(valid_length as u64)?;
    }
    file.seek(SeekFrom::End(0))?;
    Ok((file, contents))
}

pub fn record_rosetta_fallback(binary_path: String, os_version: String) -> Result<()> {
    let (mut file, contents) = open_rosetta_fallbacks_for_append()?;
    let binary_path_json = serde_json::to_string(&binary_path)?;
    let binary_path_field = format!("\"binary_path\":{binary_path_json}");
    if contents
        .windows(binary_path_field.len())
        .any(|window| window == binary_path_field.as_bytes())
    {
        return Ok(());
    }

    let mut entry = serde_json::to_vec(&StoredRosettaFallback {
        binary_path,
        os_version: Some(os_version),
        reported: false,
    })?;
    entry.push(b'\n');
    file.write_all(&entry)?;
    file.sync_data()?;
    Ok(())
}

pub fn load_rosetta_fallbacks() -> Result<Vec<RosettaFallbackEntry>> {
    let path = rosetta_fallbacks_path()?;
    let mut file = match OpenOptions::new().read(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    file.lock_shared()?;

    let mut contents = Vec::new();
    file.read_to_end(&mut contents)?;
    let complete_length = contents
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    let mut entries = BTreeMap::<String, RosettaFallbackEntry>::new();
    for line in contents
        .get(..complete_length)
        .expect("complete length ends at a newline")
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let entry: StoredRosettaFallback = serde_json::from_slice(line)?;
        if entry.reported {
            entries.remove(&entry.binary_path);
        } else {
            let os_version = entry.os_version.ok_or_else(|| {
                SipError::UnlikelyError(format!(
                    "Rosetta fallback entry for {} has no macOS version",
                    entry.binary_path
                ))
            })?;
            entries
                .entry(entry.binary_path.clone())
                .or_insert(RosettaFallbackEntry {
                    binary_path: entry.binary_path,
                    os_version,
                });
        }
    }
    Ok(entries.into_values().collect())
}

pub fn mark_rosetta_fallbacks_reported(reported: &[RosettaFallbackEntry]) -> Result<()> {
    let (mut file, contents) = open_rosetta_fallbacks_for_append()?;
    let mut reported_paths = HashSet::new();
    for line in contents
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let entry: StoredRosettaFallback = serde_json::from_slice(line)?;
        if entry.reported {
            reported_paths.insert(entry.binary_path);
        }
    }

    let mut wrote_marker = false;
    for entry in reported {
        if reported_paths.insert(entry.binary_path.clone()) {
            let mut marker = serde_json::to_vec(&StoredRosettaFallback {
                binary_path: entry.binary_path.clone(),
                os_version: None,
                reported: true,
            })?;
            marker.push(b'\n');
            file.write_all(&marker)?;
            wrote_marker = true;
        }
    }

    if wrote_marker {
        file.sync_data()?;
    }
    Ok(())
}
