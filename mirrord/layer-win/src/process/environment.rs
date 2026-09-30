//! Windows environment block parsing utilities
//!
//! This module provides safe, robust parsing of Windows environment blocks
//! with comprehensive edge case handling based on
//! <https://nullprogram.com/blog/2023/08/23/> analysis.
//!
//! # Names compare like Windows compares them
//!
//! Windows treats environment names case-insensitively: `Path` and `PATH` are one variable. It
//! compares them ordinally, code unit by code unit over UTF-16, after mapping each unit through
//! the system's uppercase table, and keeps the block sorted in that order. [`windows_name_order`]
//! asks the system for exactly that comparison. Rust's `to_uppercase` is not a substitute: it
//! works on `char`s and can expand one into several (`ß` becomes `SS`), which the system table
//! never does.

use std::{cmp::Ordering, collections::HashMap, ffi::c_void};

use str_win::{MultiBufferChar, find_multi_buffer_safe_len, multi_buffer_to_strings};
use winapi::{
    shared::minwindef::{BOOL, TRUE},
    um::winbase::CREATE_UNICODE_ENVIRONMENT,
};

// `CompareStringOrdinal` results, from `stringapiset.h`.
const CSTR_LESS_THAN: i32 = 1;
const CSTR_EQUAL: i32 = 2;
const CSTR_GREATER_THAN: i32 = 3;

#[link(name = "kernel32")]
unsafe extern "system" {
    fn CompareStringOrdinal(
        string1: *const u16,
        count1: i32,
        string2: *const u16,
        count2: i32,
        ignore_case: BOOL,
    ) -> i32;
}

/// Orders two environment names the way Windows orders the entries of an environment block:
/// ordinally over UTF-16, ignoring case through the system's uppercase table.
///
/// `=C:` sorts before every letter, because `=` is below them.
///
/// # Arguments
///
/// * `left`, `right` - the names, as UTF-16 code units without a terminator.
pub fn windows_name_order(left: &[u16], right: &[u16]) -> Ordering {
    let (Ok(left_len), Ok(right_len)) = (i32::try_from(left.len()), i32::try_from(right.len()))
    else {
        return left.cmp(right);
    };

    match unsafe { CompareStringOrdinal(left.as_ptr(), left_len, right.as_ptr(), right_len, TRUE) }
    {
        CSTR_LESS_THAN => Ordering::Less,
        CSTR_EQUAL => Ordering::Equal,
        CSTR_GREATER_THAN => Ordering::Greater,
        // Fails only on invalid arguments, which two slices cannot be.
        _ => left.cmp(right),
    }
}

/// What a process creation asked the child's environment to be.
#[derive(Debug, PartialEq, Eq)]
pub enum CallerEnvironment {
    /// No block: the child inherits this process's environment.
    Inherit,
    /// The caller's own block. Empty when the caller asked for an empty environment, which is
    /// not the same as inheriting one.
    Explicit(HashMap<String, String>),
}

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
/// An empty map stands for both "inherit" (a null block) and an explicitly empty block. Use
/// [`parse_caller_environment`] where the difference matters.
///
/// # Safety
/// This function is unsafe because it dereferences raw pointers from the Windows API.
/// The caller must ensure that the environment pointer is valid and properly formatted.
pub unsafe fn parse_environment_block(
    environment: *mut c_void,
    creation_flags: u32,
) -> HashMap<String, String> {
    match unsafe { parse_caller_environment(environment, creation_flags) } {
        CallerEnvironment::Inherit => HashMap::new(),
        CallerEnvironment::Explicit(environment) => environment,
    }
}

/// Parse the environment block a process creation was given.
///
/// The environment block can be either ANSI or Unicode depending on the creation flags.
/// This function checks the CREATE_UNICODE_ENVIRONMENT flag to determine the format.
///
/// A name that appears more than once, in any mix of case, keeps its first entry. That is the
/// entry the child's `GetEnvironmentVariable` finds, because it scans the block from the start.
///
/// # Returns
///
/// [`CallerEnvironment::Inherit`] for a null block, and for a block with no terminator in reach,
/// which cannot be read safely. [`CallerEnvironment::Explicit`] otherwise, empty for a block that
/// holds only its terminator.
///
/// # Safety
/// This function is unsafe because it dereferences raw pointers from the Windows API.
/// The caller must ensure that the environment pointer is valid and properly formatted.
pub unsafe fn parse_caller_environment(
    environment: *mut c_void,
    creation_flags: u32,
) -> CallerEnvironment {
    if environment.is_null() {
        return CallerEnvironment::Inherit;
    }

    let parsed = if creation_flags & CREATE_UNICODE_ENVIRONMENT != 0 {
        unsafe { parse_environment_block_typed::<u16>(environment) }
    } else {
        unsafe { parse_environment_block_typed::<u8>(environment) }
    };

    match parsed {
        Some(environment) => CallerEnvironment::Explicit(environment),
        None => {
            tracing::warn!("environment block has no terminator in reach, inheriting instead");
            CallerEnvironment::Inherit
        }
    }
}

/// Type-specific environment block parser - eliminates duplicate unsafe calls
///
/// # Returns
///
/// `None` when no double-NUL terminator is found in reach.
unsafe fn parse_environment_block_typed<T: MultiBufferChar>(
    environment: *mut c_void,
) -> Option<HashMap<String, String>> {
    let env_ptr = environment as *const T;

    // Find the actual size using str-win safe length utility
    let final_size = match unsafe { find_multi_buffer_safe_len(env_ptr, 65536) }? {
        // Valid environment (more than just double null)
        size if size > 2 => size,
        // Only the terminator: the caller asked for no variables at all.
        _ => return Some(HashMap::new()),
    };

    // Create slice with actual found size
    let env_slice = unsafe { std::slice::from_raw_parts(env_ptr, final_size) };

    // Parse environment strings using str-win utilities
    let raw_strings = multi_buffer_to_strings(env_slice);

    // Filter out invalid entries with logging
    let valid_strings = filter_environment_strings::<T>(raw_strings);

    Some(build_environment_map(valid_strings))
}

/// Build HashMap from validated environment strings
///
/// Names are compared like Windows compares them (see [`windows_name_order`]), and the first
/// entry of a name wins over any later one, whatever its case.
fn build_environment_map(env_strings: Vec<String>) -> HashMap<String, String> {
    let mut entries = env_strings
        .into_iter()
        .filter_map(|env_string| {
            let name = environment_name_of(&env_string)?;
            let wide_name = name.encode_utf16().collect::<Vec<_>>();
            let value = env_string[name.len() + 1..].to_owned();
            Some((wide_name, name.to_owned(), value))
        })
        .collect::<Vec<_>>();

    // A stable sort keeps block order among equal names, so `dedup_by` keeps the first entry.
    entries.sort_by(|(left, ..), (right, ..)| windows_name_order(left, right));
    entries.dedup_by(|(later, later_name, _), (first, first_name, _)| {
        let duplicate = windows_name_order(later, first) == Ordering::Equal;
        if duplicate {
            tracing::debug!(
                kept = first_name.as_str(),
                dropped = later_name.as_str(),
                "environment block names one variable twice, keeping the first entry"
            );
        }
        duplicate
    });

    entries
        .into_iter()
        .map(|(_, name, value)| (name, value))
        .collect()
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
            .expect("an empty block is terminated")
    }

    /// Encodes `entries` as a Unicode environment block, in the given order.
    fn unicode_block(entries: &[&str]) -> Vec<u16> {
        let mut block = entries
            .iter()
            .flat_map(|entry| entry.encode_utf16().chain([0]))
            .collect::<Vec<_>>();
        block.push(0);
        if entries.is_empty() {
            block.push(0);
        }
        block
    }

    fn wide(name: &str) -> Vec<u16> {
        name.encode_utf16().collect()
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
            unsafe { parse_environment_block_typed::<u16>(env_u16.as_mut_ptr() as *mut c_void) }
                .expect("terminated");

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
            unsafe { parse_environment_block_typed::<u8>(env_data.as_mut_ptr() as *mut c_void) }
                .expect("terminated");

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
            unsafe { parse_environment_block_typed::<u16>(env_u16.as_mut_ptr() as *mut c_void) }
                .expect("terminated");

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

    /// A null block inherits; a block that holds only its terminator asks for no variables.
    #[test]
    fn an_empty_block_is_not_an_inherited_one() {
        assert_eq!(
            unsafe { parse_caller_environment(std::ptr::null_mut(), CREATE_UNICODE_ENVIRONMENT) },
            CallerEnvironment::Inherit
        );

        let mut empty = unicode_block(&[]);
        assert_eq!(empty, [0, 0], "an empty block is two NULs");
        assert_eq!(
            unsafe {
                parse_caller_environment(
                    empty.as_mut_ptr() as *mut c_void,
                    CREATE_UNICODE_ENVIRONMENT,
                )
            },
            CallerEnvironment::Explicit(HashMap::new())
        );

        let mut ansi_empty = [0u8, 0u8];
        assert_eq!(
            unsafe { parse_caller_environment(ansi_empty.as_mut_ptr() as *mut c_void, 0) },
            CallerEnvironment::Explicit(HashMap::new())
        );
    }

    /// Names that differ only in case are one variable, and the first entry in the block wins.
    #[test]
    fn a_name_repeated_in_another_case_keeps_its_first_entry() {
        let mut block = unicode_block(&[
            "Path=C:/first",
            "OTHER=x",
            "PATH=C:/second",
            "path=C:/third",
        ]);

        let CallerEnvironment::Explicit(map) = (unsafe {
            parse_caller_environment(
                block.as_mut_ptr() as *mut c_void,
                CREATE_UNICODE_ENVIRONMENT,
            )
        }) else {
            panic!("an explicit block");
        };

        assert_eq!(map.len(), 2, "{map:?}");
        assert_eq!(map.get("Path").map(String::as_str), Some("C:/first"));
        assert_eq!(map.get("OTHER").map(String::as_str), Some("x"));
    }

    /// The order is Windows' own: `=` entries first, then ordinal over UTF-16 ignoring case.
    #[test]
    fn names_order_like_windows() {
        let mut names = ["path", "=C:", "Windir", "_underscore", "=D:", "A", "PATH2"]
            .map(wide)
            .to_vec();
        names.sort_by(|left, right| windows_name_order(left, right));

        let names = names
            .iter()
            .map(|name| String::from_utf16_lossy(name))
            .collect::<Vec<_>>();
        // `_` (0x5F) sorts after the uppercase letters it is compared against, like Windows.
        assert_eq!(
            names,
            ["=C:", "=D:", "A", "path", "PATH2", "Windir", "_underscore"]
        );
    }

    /// Case folding is per UTF-16 code unit through the system table, not Rust's expanding
    /// `to_uppercase`: `ß` is not `SS`, while `é` and `É` are one name.
    #[test]
    fn case_folding_does_not_expand() {
        assert_eq!(windows_name_order(&wide("é"), &wide("É")), Ordering::Equal);
        assert_ne!(windows_name_order(&wide("ß"), &wide("SS")), Ordering::Equal);
        assert_eq!(
            windows_name_order(&wide("path"), &wide("PATH")),
            Ordering::Equal
        );
    }
}
