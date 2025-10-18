pub fn u8_buffer_to_string<T: AsRef<[u8]>>(buffer: T) -> String {
    let buffer = buffer.as_ref();

    // Find the first null byte (0) or use the entire buffer
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());

    // Convert to string, handling invalid UTF-8 gracefully
    String::from_utf8_lossy(buffer.get(..len).unwrap_or(buffer)).into_owned()
}

pub fn u16_buffer_to_string<T: AsRef<[u16]>>(buffer: T) -> String {
    let buffer = buffer.as_ref();

    // Find the first null u16 (0) or use the entire buffer
    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());

    // Convert u16 slice to string by treating each u16 as a Unicode code point
    // This is a simplified approach - real UTF-16 would be more complex
    buffer
        .get(..len)
        .unwrap_or(buffer)
        .iter()
        .filter_map(|&c| std::char::from_u32(c as u32))
        .collect()
}

pub fn u8_multi_buffer_to_strings<T: AsRef<[u8]>>(buffer: T) -> Vec<String> {
    let buffer = buffer.as_ref();

    if buffer.is_empty() {
        return vec![];
    }

    let mut result = Vec::new();
    let mut start = 0;

    for (i, &c) in buffer.iter().enumerate() {
        if c == 0 {
            if start < i {
                if let Some(slice) = buffer.get(start..i) {
                    if let Ok(substring) = String::from_utf8(slice.to_vec()) {
                        if !substring.is_empty() {
                            result.push(substring);
                        }
                    } else {
                        // Fallback: lossy decode if invalid UTF-8
                        result.push(String::from_utf8_lossy(slice).into_owned());
                    }
                }
            }
            start = i + 1;

            // Check for double null terminator (end of MULTI_SZ)
            if buffer.get(i + 1).copied().unwrap_or(1) == 0 {
                break;
            }
        }
    }

    result
}

pub fn u16_multi_buffer_to_strings<T: AsRef<[u16]>>(buffer: T) -> Vec<String> {
    let buffer = buffer.as_ref();

    if buffer.is_empty() {
        return vec![];
    }

    let mut result = Vec::new();
    let mut start = 0;

    for (i, &c) in buffer.iter().enumerate() {
        if c == 0 {
            if start < i {
                // Convert the substring to String
                if let Some(slice) = buffer.get(start..i) {
                    let substring: String = slice
                        .iter()
                        .filter_map(|&c| std::char::from_u32(c as u32))
                        .collect();
                    if !substring.is_empty() {
                        result.push(substring);
                    }
                }
            }
            start = i + 1;

            // Check for double null terminator (end of MULTI_SZ)
            if buffer.get(i + 1).copied().unwrap_or(1) == 0 {
                break;
            }
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

/// Convert a null-terminated wide string pointer (LPCWSTR) to a Rust String.
///
/// This function safely handles Windows API wide string pointers by:
/// 1. Checking for null pointers
/// 2. Finding the null terminator
/// 3. Using proper UTF-16 decoding
///
/// # Safety
///
/// The caller must ensure that `ptr` is a valid pointer to a null-terminated
/// wide string, or a null pointer.
///
/// # Arguments
///
/// * `ptr` - A pointer to a null-terminated wide string (LPCWSTR) or null
///
/// # Returns
///
/// * `Some(String)` - If the pointer is valid and can be converted
/// * `None` - If the pointer is null
pub unsafe fn lpcwstr_to_string(ptr: *const u16) -> Option<String> {
    if ptr.is_null() {
        return None;
    }

    // Find the length by counting until null terminator
    let len = (0..)
        .take_while(|&i| unsafe { *ptr.offset(i) != 0 })
        .count();

    // Create slice and convert using proper UTF-16 decoding
    let slice = unsafe { std::slice::from_raw_parts(ptr, len) };
    Some(String::from_utf16_lossy(slice))
}

/// Convert a null-terminated wide string pointer (LPCWSTR) to a Rust String,
/// returning an empty string for null pointers.
///
/// This is a convenience wrapper around `lpcwstr_to_string` that never returns None.
///
/// # Safety
///
/// The caller must ensure that `ptr` is a valid pointer to a null-terminated
/// wide string, or a null pointer.
///
/// # Arguments
///
/// * `ptr` - A pointer to a null-terminated wide string (LPCWSTR) or null
///
/// # Returns
///
/// * `String` - The converted string, or empty string if pointer is null
pub unsafe fn lpcwstr_to_string_or_empty(ptr: *const u16) -> String {
    unsafe { lpcwstr_to_string(ptr) }.unwrap_or_default()
}
