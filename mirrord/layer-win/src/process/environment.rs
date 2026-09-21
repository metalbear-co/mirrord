//! Windows environment block parsing utilities
//!
//! This module provides safe, robust parsing of Windows environment blocks
//! with comprehensive edge case handling based on
//! <https://nullprogram.com/blog/2023/08/23/> analysis.

use std::{collections::HashMap, ffi::c_void};

use str_win::{MultiBufferChar, find_multi_buffer_safe_len, multi_buffer_to_strings};
use winapi::um::winbase::CREATE_UNICODE_ENVIRONMENT;

/// Filter environment strings according to Windows requirements and log invalid entries
///
/// An entry is valid when it is not empty and carries a non-empty name before a `=`.
///
/// An entry that starts with `=` is valid and must be kept. Windows records the current directory
/// of each drive in the environment block, as `=C:=C:/work`, and the C runtime reads them back to
/// resolve a drive-relative path. Dropping them changes where a child process resolves such a path.
fn filter_environment_strings<T: MultiBufferChar>(strings: Vec<String>) -> Vec<String> {
    let original_count = strings.len();
    let mut filtered_strings = Vec::new();
    let mut filtered_count = 0;

    let type_name = if std::mem::size_of::<T>() == 2 {
        "Unicode"
    } else {
        "ANSI"
    };

    for s in strings {
        if environment_name_of(&s).is_some() {
            filtered_strings.push(s);
        } else {
            filtered_count += 1;
            tracing::warn!(
                "Filtered out invalid environment entry ({}): {:?}",
                type_name,
                s
            );
        }
    }

    if filtered_count > 0 {
        tracing::warn!(
            "Filtered {} invalid environment entries out of {} total ({})",
            filtered_count,
            original_count,
            type_name
        );
    }

    filtered_strings
}

/// Parse Windows environment block into HashMap
///
/// The environment block can be either ANSI or Unicode depending on the creation flags.
/// This function checks the CREATE_UNICODE_ENVIRONMENT flag to determine the format.
///
/// # Safety
/// This function is unsafe because it dereferences raw pointers from the Windows API.
/// The caller must ensure that the environment pointer is valid and properly formatted.
pub unsafe fn parse_environment_block(
    environment: *mut c_void,
    creation_flags: u32,
) -> HashMap<String, String> {
    if environment.is_null() {
        return HashMap::new();
    }

    if creation_flags & CREATE_UNICODE_ENVIRONMENT != 0 {
        unsafe { parse_environment_block_typed::<u16>(environment) }
    } else {
        unsafe { parse_environment_block_typed::<u8>(environment) }
    }
}

/// Type-specific environment block parser - eliminates duplicate unsafe calls
unsafe fn parse_environment_block_typed<T: MultiBufferChar>(
    environment: *mut c_void,
) -> HashMap<String, String> {
    let env_ptr = environment as *const T;

    // Find the actual size using str-win safe length utility
    let final_size = match unsafe { find_multi_buffer_safe_len(env_ptr, 65536) } {
        // Valid environment (more than just double null)
        Some(size) if size > 2 => size,
        // Empty or malformed environment
        _ => return HashMap::new(),
    };

    // Create slice with actual found size
    let env_slice = unsafe { std::slice::from_raw_parts(env_ptr, final_size) };

    // Parse environment strings using str-win utilities
    let raw_strings = multi_buffer_to_strings(env_slice);

    // Filter out invalid entries with logging
    let valid_strings = filter_environment_strings::<T>(raw_strings);

    // Convert to HashMap
    // Note: Windows maintains invariant that duplicate variables have same value
    build_environment_map(valid_strings)
}

/// Build HashMap from validated environment strings
fn build_environment_map(env_strings: Vec<String>) -> HashMap<String, String> {
    let mut env_map = HashMap::new();
    for env_string in env_strings {
        if let Some(name) = environment_name_of(&env_string) {
            let value = env_string[name.len() + 1..].to_owned();
            env_map.insert(name.to_owned(), value);
        }
    }
    env_map
}

/// Returns the name part of an environment entry, without the `=` that ends it.
///
/// A leading `=` belongs to the name, not to the separator. Windows records the current directory
/// of a drive as `=C:=C:/work`, whose name is `=C:`.
///
/// # Arguments
///
/// * `entry` - one `name=value` entry from an environment block.
///
/// # Returns
///
/// The name, or `None` when the entry has no separator or an empty name.
fn environment_name_of(entry: &str) -> Option<&str> {
    let start = usize::from(entry.starts_with('='));
    let separator = entry[start..].find('=')? + start;

    (separator > start).then(|| &entry[..separator])
}

#[cfg(test)]
mod tests {
    use std::ffi::c_void;

    use super::*;

    /// Helper to test empty environment parsing for any character type
    fn test_empty_environment<T: MultiBufferChar>() -> HashMap<String, String> {
        let empty_env: [T; 2] = [T::default(), T::default()];
        unsafe { parse_environment_block_typed::<T>(empty_env.as_ptr() as *mut c_void) }
    }

    #[test]
    fn test_empty_environment_special_case() {
        // Test empty environment for both u8 and u16
        let result_u16 = test_empty_environment::<u16>();
        assert!(result_u16.is_empty());

        let result_u8 = test_empty_environment::<u8>();
        assert!(result_u8.is_empty());
    }

    #[test]
    fn test_invalid_entries_filtered() {
        // Test entries that should be filtered out per Windows rules
        let invalid_entries = [
            "=INVALID", // Starts with =
            "NOEQUALS", // No = character
            "",         // Empty string
        ];

        for entry in &invalid_entries {
            let bytes = entry.as_bytes();
            assert!(!bytes.is_empty() || !entry.starts_with('=') || !entry.contains('='));
        }
    }

    #[test]
    fn test_warning_for_invalid_entries() {
        // Test that we generate warnings for invalid entries in Unicode parsing
        let mut env_u16: Vec<u16> = Vec::new();

        // Add valid entry
        for c in "VALID=value".encode_utf16() {
            env_u16.push(c);
        }
        env_u16.push(0);

        // Add invalid entry that starts with =
        for c in "=INVALID".encode_utf16() {
            env_u16.push(c);
        }
        env_u16.push(0);

        // Add invalid entry without =
        for c in "NOEQUALS".encode_utf16() {
            env_u16.push(c);
        }
        env_u16.push(0);

        env_u16.push(0); // Double null terminator

        let result =
            unsafe { parse_environment_block_typed::<u16>(env_u16.as_mut_ptr() as *mut c_void) };

        // Should only contain the valid entry
        assert_eq!(result.len(), 1);
        assert_eq!(result.get("VALID"), Some(&"value".to_owned()));

        // Invalid entries should have been filtered out and warnings logged
        assert!(!result.contains_key("=INVALID"));
        assert!(!result.contains_key("NOEQUALS"));
    }

    #[test]
    fn test_malformed_utf8_handling() {
        // Test that we handle invalid UTF-8 gracefully
        let mut env_data = vec![
            b'V', b'A', b'R', b'=', 0xFF, 0xFE, // Invalid UTF-8 sequence
            0,    // Null terminator
            0, 0, // Double null terminator
        ];

        let result =
            unsafe { parse_environment_block_typed::<u8>(env_data.as_mut_ptr() as *mut c_void) };

        // Should still parse the variable name even with invalid UTF-8 value
        assert!(result.contains_key("VAR"));
    }

    #[test]
    fn test_normal_environment_parsing() {
        // Test normal case with Unicode
        let mut env_u16: Vec<u16> = Vec::new();

        // "PATH=C:\\bin\0USER=test\0\0"
        for c in "PATH=C:\\bin".encode_utf16() {
            env_u16.push(c);
        }
        env_u16.push(0);
        for c in "USER=test".encode_utf16() {
            env_u16.push(c);
        }
        env_u16.push(0);
        env_u16.push(0); // Double null terminator

        let result =
            unsafe { parse_environment_block_typed::<u16>(env_u16.as_mut_ptr() as *mut c_void) };

        assert_eq!(result.len(), 2);
        assert_eq!(result.get("PATH"), Some(&"C:\\bin".to_owned()));
        assert_eq!(result.get("USER"), Some(&"test".to_owned()));
    }

    #[test]
    fn keeps_the_current_directory_of_each_drive() {
        let entries = vec![
            "=C:=C:/work".to_owned(),
            "=D:=D:/".to_owned(),
            "=ExitCode=00000000".to_owned(),
            "PATH=C:/Windows".to_owned(),
        ];

        let map = build_environment_map(filter_environment_strings::<u16>(entries));

        assert_eq!(map.get("=C:").map(String::as_str), Some("C:/work"));
        assert_eq!(map.get("=D:").map(String::as_str), Some("D:/"));
        assert_eq!(map.get("=ExitCode").map(String::as_str), Some("00000000"));
        assert_eq!(map.get("PATH").map(String::as_str), Some("C:/Windows"));
    }

    #[test]
    fn drops_entries_without_a_name() {
        let entries = vec![
            "".to_owned(),
            "no-separator".to_owned(),
            "=".to_owned(),
            "==value".to_owned(),
            "PATH=C:/Windows".to_owned(),
        ];

        let map = build_environment_map(filter_environment_strings::<u16>(entries));

        assert_eq!(map.len(), 1, "only PATH is a valid entry: {map:?}");
        assert!(map.contains_key("PATH"));
    }

    #[test]
    fn keeps_an_empty_value() {
        let map =
            build_environment_map(filter_environment_strings::<u16>(vec!["EMPTY=".to_owned()]));

        assert_eq!(map.get("EMPTY").map(String::as_str), Some(""));
    }

    #[test]
    fn keeps_a_value_that_holds_separators() {
        let map = build_environment_map(filter_environment_strings::<u16>(vec![
            "PATH=C:/a;C:/b=c".to_owned(),
        ]));

        assert_eq!(map.get("PATH").map(String::as_str), Some("C:/a;C:/b=c"));
    }
}
