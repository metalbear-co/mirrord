use std::path::{Path, PathBuf};

/// Find the length of a null-terminated buffer using an efficient search
///
/// This utility function replaces the common pattern of using `(0..).take_while`
/// to find null terminators in buffers.
///
/// # Arguments
///
/// * `buffer` - The buffer to search
/// * `null_value` - The value to search for (usually 0)
///
/// # Returns
///
/// The index of the first occurrence of `null_value`, or the buffer length if not found
fn find_null_terminator_length<T: PartialEq>(buffer: &[T], null_value: T) -> usize {
    buffer
        .iter()
        .position(|x| *x == null_value)
        .unwrap_or(buffer.len())
}

pub fn u8_buffer_to_string<T: AsRef<[u8]>>(buffer: T) -> String {
    let buffer = buffer.as_ref();

    // Find the first null byte (0) using utility function
    let len = find_null_terminator_length(buffer, 0);

    // Convert to string, handling invalid UTF-8 gracefully
    String::from_utf8_lossy(buffer.get(..len).unwrap_or(buffer)).into_owned()
}

pub fn u16_buffer_to_string<T: AsRef<[u16]>>(buffer: T) -> String {
    let buffer = buffer.as_ref();

    // Find the first null u16 (0) using utility function
    let len = find_null_terminator_length(buffer, 0);

    // Convert u16 slice to string by treating each u16 as a Unicode code point
    // This is a simplified approach - real UTF-16 would be more complex
    buffer
        .get(..len)
        .unwrap_or(buffer)
        .iter()
        .filter_map(|&c| std::char::from_u32(c as u32))
        .collect()
}

/// Trait for characters that can be parsed from multi-buffer format
pub trait MultiBufferChar: Copy + PartialEq + Default {
    /// Convert a slice of this character type to a String
    fn slice_to_string(slice: &[Self]) -> String;
}

impl MultiBufferChar for u8 {
    fn slice_to_string(slice: &[Self]) -> String {
        if let Ok(substring) = String::from_utf8(slice.to_vec()) {
            substring
        } else {
            // Fallback: lossy decode if invalid UTF-8
            String::from_utf8_lossy(slice).into_owned()
        }
    }
}

impl MultiBufferChar for u16 {
    fn slice_to_string(slice: &[Self]) -> String {
        slice
            .iter()
            .filter_map(|&c| std::char::from_u32(c as u32))
            .collect()
    }
}

/// Multi-buffer parser for both u8 and u16 character types
pub fn multi_buffer_to_strings<T: MultiBufferChar>(buffer: &[T]) -> Vec<String> {
    if buffer.is_empty() {
        return vec![];
    }

    let mut result = Vec::new();
    let mut start = 0;

    while let Some(remaining) = buffer.get(start..) {
        if remaining.is_empty() {
            break;
        }

        // Find the length of current string using utility function
        let len = find_null_terminator_length(remaining, T::default());

        if len > 0
            && let Some(slice) = remaining.get(..len)
        {
            let substring = T::slice_to_string(slice);
            if !substring.is_empty() {
                result.push(substring);
            }
        }

        // Move past the null terminator (or to the end if none was found)
        start = start.saturating_add(len).saturating_add(1);

        // Check for double null terminator (end of MULTI_SZ)
        match buffer.get(start) {
            Some(value) if *value == T::default() => break,
            None => break,
            _ => {}
        }
    }

    result
}

pub fn string_to_u8_buffer<T: AsRef<str>>(string: T) -> Vec<u8> {
    let mut bytes = string.as_ref().as_bytes().to_vec();
    bytes.push(0); // Add null terminator
    bytes
}

pub fn string_to_u16_buffer<T: AsRef<str>>(string: T) -> Vec<u16> {
    string
        .as_ref()
        .chars()
        // Simple conversion - not proper UTF-16
        .map(|c| c as u16)
        .chain(Some(0))
        .collect()
}

/// Convert a null-terminated C string pointer to a Rust String.
///
/// This function safely converts a C-style string (char*) to a Rust String
/// by finding the null terminator and converting the resulting slice.
///
/// # Safety
///
/// The caller must ensure that `ptr` points to a valid null-terminated C string.
/// The function will read memory until it finds a null terminator (0).
///
/// # Arguments
///
/// * `ptr` - A pointer to a null-terminated C string (char array)
///
/// # Returns
///
/// A Rust String containing the converted text, or an empty string if the pointer is null.
pub unsafe fn u8_ptr_to_string(ptr: *const i8) -> String {
    if ptr.is_null() {
        return String::new();
    }

    // Safely determine a reasonable maximum length for the search
    const MAX_C_STRING_LEN: usize = 32768; // 32KB should be plenty for most use cases

    // Create a slice with maximum safe length, then find the null terminator
    let c_slice = unsafe { std::slice::from_raw_parts(ptr as *const u8, MAX_C_STRING_LEN) };
    let len = find_null_terminator_length(c_slice, 0);

    if len == 0 {
        return String::new();
    }

    // Create a slice from the pointer and length, then convert
    let c_slice = unsafe { std::slice::from_raw_parts(ptr as *const u8, len) };
    u8_buffer_to_string(c_slice)
}

/// Convert a null-terminated wide string pointer to a Rust String.
///
/// This function safely converts a Windows-style wide string (LPCWSTR) to a Rust String
/// by finding the null terminator and converting the resulting slice.
///
/// # Safety
///
/// The caller must ensure that `ptr` points to a valid null-terminated wide string.
/// The function will read memory until it finds a null terminator (0).
///
/// # Arguments
///
/// * `ptr` - A pointer to a null-terminated wide string (u16 array)
///
/// # Returns
///
/// A Rust String containing the converted text, or an empty string if the pointer is null.
pub unsafe fn u16_ptr_to_string(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }

    // Safely determine a reasonable maximum length for the search
    const MAX_WIDE_STRING_LEN: usize = 32768; // 32KB should be plenty for most use cases

    // Create a slice with maximum safe length, then find the null terminator
    let wide_slice = unsafe { std::slice::from_raw_parts(ptr, MAX_WIDE_STRING_LEN) };
    let len = find_null_terminator_length(wide_slice, 0);

    if len == 0 {
        return String::new();
    }

    // Create a slice from the pointer and length, then convert
    let wide_slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    u16_buffer_to_string(wide_slice)
}

/// Find the safe length of a multi-buffer by locating the double null terminator
///
/// This function safely searches for the end of a multi-buffer (MULTI_SZ format)
/// by finding the double null terminator pattern. It bounds the search to prevent
/// reading beyond reasonable memory limits.
///
/// # Safety
///
/// The caller must ensure that `ptr` points to a valid multi-buffer or null pointer.
/// The function will not read beyond `max_bytes` to maintain safety.
///
/// # Arguments
///
/// * `ptr` - A pointer to the start of a multi-buffer
/// * `max_bytes` - Maximum number of bytes to search (safety limit)
///
/// # Returns
///
/// * `Some(usize)` - The length including the double null terminator if found
/// * `None` - If double null terminator not found within max_bytes or ptr is null
pub unsafe fn find_multi_buffer_safe_len<T: MultiBufferChar>(
    ptr: *const T,
    max_bytes: usize,
) -> Option<usize> {
    if ptr.is_null() {
        return None;
    }

    let max_elements = max_bytes / std::mem::size_of::<T>();

    // Create a slice with the maximum safe length to search within
    let slice = unsafe { std::slice::from_raw_parts(ptr, max_elements) };

    // Search for double null terminator pattern using our utility function
    let mut pos = 0;
    while let Some(remaining_slice) = slice.get(pos..) {
        if remaining_slice.is_empty() {
            break;
        }

        let next_null = find_null_terminator_length(remaining_slice, T::default());

        if next_null == 0 {
            // Found a null at current position, check if next is also null (double null)
            if let Some(next_value) = slice.get(pos + 1) {
                if *next_value == T::default() {
                    // Found double null terminator
                    return Some(pos + 2);
                }
            } else {
                break;
            }
            // Single null, move past it
            pos += 1;
        } else {
            // Move past this string and its null terminator
            pos = pos.saturating_add(next_null).saturating_add(1);
        }
    }

    None // Double null terminator not found within bounds
}

// This prefix is a way to explicitly indicate that we're looking in
// the global namespace for a path.
const GLOBAL_NAMESPACE_PATH: &str = r#"\??\"#;

/// Responsible for turning a Windows absolute path (potentially Device path) into a Unix-compatible
/// path.
///
/// ## Implementation
///
/// 1. If present, remove global namespace path, for device paths (e.g. `"\\??\\"`.)
/// 2. Remove the volume from the path.
/// 3. Make all back-slashes be forward-slashes.
///
/// # Arguments
///
/// * `path` - A Windows absolute path.
pub fn path_to_unix_path<T: AsRef<Path>>(path: T) -> Option<String> {
    let mut path = path.as_ref();

    if !path.has_root() {
        return None;
    }

    // Rust doesn't know how to separate the components in this case.
    if path.starts_with(GLOBAL_NAMESPACE_PATH) {
        path = path.strip_prefix(GLOBAL_NAMESPACE_PATH).ok()?;
    }

    // Skip root dir
    let new_path: PathBuf = path.components().skip(1).collect();

    // Turn to string, replace Windows slashes to Linux slashes for ease of use.
    Some(new_path.to_str()?.to_string().replace("\\", "/"))
}
