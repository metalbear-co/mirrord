//! Cross-platform process environment utilities.
//!
//! This module provides functionality for handling process environment variables,
//! with specialized support for Windows environment blocks.

use std::{collections::HashMap, ffi::OsString};

#[cfg(windows)]
use str_win::{string_to_u16_buffer, u8_multi_buffer_to_strings, u16_multi_buffer_to_strings};
#[cfg(windows)]
use winapi::shared::minwindef::LPVOID;

/// A wrapper around HashMap for environment variables that provides Windows-specific functionality.
///
/// This type can convert environment variables into Windows environment blocks for use with
/// CreateProcess functions, and provides consistent handling of environment variable formats.
#[derive(Debug, Default)]
pub struct EnvMap(pub HashMap<OsString, OsString>);

impl<K, V> From<HashMap<K, V>> for EnvMap
where
    K: Into<String>,
    V: Into<String>,
{
    fn from(input: HashMap<K, V>) -> Self {
        let mut result = HashMap::<OsString, OsString>::with_capacity(input.len());

        for (k, v) in input {
            result.insert(OsString::from(k.into()), OsString::from(v.into()));
        }
        EnvMap(result)
    }
}

impl EnvMap {
    /// Creates a new empty EnvMap
    pub fn new() -> Self {
        Self::default()
    }

    /// Converts the environment map to a Windows environment block.
    ///
    /// The Windows environment block is a null-terminated list of null-terminated strings,
    /// sorted case-insensitively, encoded as UTF-16 for use with CreateProcessW.
    ///
    /// # Returns
    ///
    /// A Vec<u16> containing the UTF-16 encoded environment block suitable for Windows
    /// CreateProcess APIs.
    #[cfg(windows)]
    pub fn to_windows_env_block(&self) -> Vec<u16> {
        let mut entries: Vec<(String, String, String)> = self
            .0
            .iter()
            .map(|(key, val)| {
                let key_string = key.to_string_lossy().into_owned();
                let lower_key = key_string.to_lowercase();
                let value_string = val.to_string_lossy().into_owned();
                (lower_key, key_string, value_string)
            })
            .collect();

        // The Windows environment block is expected to be sorted case-insensitively.
        entries.sort_by(|(lower_a, key_a, _), (lower_b, key_b, _)| {
            lower_a.cmp(lower_b).then_with(|| key_a.cmp(key_b))
        });

        let mut block: Vec<u16> = Vec::new();

        for (_, key, value) in entries {
            let env_entry = format!("{key}={value}");
            let entry_u16 = string_to_u16_buffer(&env_entry);
            block.extend(entry_u16);
        }

        if block.is_empty() {
            block.push(0);
        }
        block.push(0);

        block
    }

    /// Stub implementation for non-Windows platforms
    #[cfg(not(windows))]
    pub fn to_windows_env_block(&self) -> Vec<u16> {
        Vec::new()
    }
}

/// Extracts environment variables from a Windows LPVOID environment block.
///
/// This function handles both UTF-8 and UTF-16 encoded environment blocks,
/// which are used by CreateProcessA and CreateProcessW respectively.
///
/// # Arguments
///
/// * `is_w16_env` - Whether the environment is UTF-16 encoded (true) or UTF-8 (false)
/// * `environment` - Pointer to the environment block
///
/// # Returns
///
/// A vector of environment variable strings in "KEY=VALUE" format
#[cfg(windows)]
pub fn environment_from_lpvoid(is_w16_env: bool, environment: LPVOID) -> Vec<String> {
    unsafe {
        let terminator_size = if is_w16_env { 4 } else { 2 };
        let environment: *mut u8 = environment as *mut u8;
        let mut environment_len = 0;

        // NOTE(gabriela): when going through `CreateProcessA`, there'd be a limit of
        // 32767 bytes. `CreateProcessW` doesn't have this, because, ridiculously, it is quite
        // common for the environment to be *way* larger than that. mirrord is a perfect example
        // which is MUCH more large. So we're gonna loop into infinity willingly. oops?
        for i in 0.. {
            let slice = std::slice::from_raw_parts(environment.add(i), terminator_size);
            if slice.iter().all(|x| *x == 0) {
                environment_len = i + terminator_size;
                break;
            }
        }

        if is_w16_env {
            let environment =
                std::slice::from_raw_parts::<u16>(environment as _, environment_len / 2);
            u16_multi_buffer_to_strings(environment)
        } else {
            let environment = std::slice::from_raw_parts(environment, environment_len);
            u8_multi_buffer_to_strings(environment)
        }
    }
}

#[cfg(not(windows))]
/// Stub implementation for non-Windows platforms
pub fn environment_from_lpvoid(
    _is_w16_env: bool,
    _environment: *mut std::ffi::c_void,
) -> Vec<String> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_envmap_creation() {
        let mut map = HashMap::new();
        map.insert("TEST_VAR".to_string(), "test_value".to_string());
        map.insert("PATH".to_string(), "/usr/bin:/bin".to_string());

        let env_map = EnvMap::from(map);
        assert_eq!(env_map.0.len(), 2);
    }

    #[test]
    fn test_envmap_default() {
        let env_map = EnvMap::default();
        assert!(env_map.0.is_empty());

        let env_map2 = EnvMap::new();
        assert!(env_map2.0.is_empty());
    }

    #[cfg(windows)]
    #[test]
    fn test_windows_env_block_creation() {
        let mut map = HashMap::new();
        map.insert("B_VAR".to_string(), "value_b".to_string());
        map.insert("A_VAR".to_string(), "value_a".to_string());

        let env_map = EnvMap::from(map);
        let block = env_map.to_windows_env_block();

        // Should not be empty
        assert!(!block.is_empty());

        // Should end with double null terminator (two zeros)
        assert_eq!(block[block.len() - 1], 0);
        assert_eq!(block[block.len() - 2], 0);

        // Convert back to string to verify sorting (A_VAR should come before B_VAR)
        let block_string = String::from_utf16_lossy(&block[..block.len() - 2]);
        assert!(block_string.contains("A_VAR=value_a"));
        assert!(block_string.contains("B_VAR=value_b"));

        // A_VAR should appear before B_VAR (case-insensitive sorting)
        let a_pos = block_string.find("A_VAR").unwrap();
        let b_pos = block_string.find("B_VAR").unwrap();
        assert!(a_pos < b_pos);
    }

    #[cfg(windows)]
    #[test]
    fn test_empty_env_block() {
        let env_map = EnvMap::new();
        let block = env_map.to_windows_env_block();

        // Empty block should just be two null terminators
        assert_eq!(block, vec![0, 0]);
    }

    #[cfg(not(windows))]
    #[test]
    fn test_non_windows_env_block() {
        let env_map = EnvMap::new();
        let block = env_map.to_windows_env_block();

        // Non-Windows should return empty vec
        assert!(block.is_empty());
    }
}
