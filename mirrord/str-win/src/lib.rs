pub fn u8_buffer_to_string<T: AsRef<[u8]>>(buffer: T) -> String {
    let buffer = buffer.as_ref();

    // Find the first null byte (0) using iterator pattern
    let len = (0..buffer.len())
        .take_while(|&i| buffer[i] != 0)
        .count();

    // Convert to string, handling invalid UTF-8 gracefully
    String::from_utf8_lossy(buffer.get(..len).unwrap_or(buffer)).into_owned()
}

pub fn u16_buffer_to_string<T: AsRef<[u16]>>(buffer: T) -> String {
    let buffer = buffer.as_ref();

    // Find the first null u16 (0) using iterator pattern
    let len = (0..buffer.len())
        .take_while(|&i| buffer[i] != 0)
        .count();

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

    while start < buffer.len() {
        // Find the length of current string using iterator pattern
        let len = (start..buffer.len())
            .take_while(|&i| buffer[i] != T::default())
            .count();
        
        if len > 0 {
            if let Some(slice) = buffer.get(start..start + len) {
                let substring = T::slice_to_string(slice);
                if !substring.is_empty() {
                    result.push(substring);
                }
            }
        }
        
        start += len + 1; // Move past the null terminator
        
        // Check for double null terminator (end of MULTI_SZ)
        if start < buffer.len() && buffer[start] == T::default() {
            break;
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
    max_bytes: usize
) -> Option<usize> {
    if ptr.is_null() {
        return None;
    }
    
    let max_elements = max_bytes / std::mem::size_of::<T>();
    let max_reasonable_size = max_elements as isize;
    
    let size = (0..max_reasonable_size)
        .take_while(|&i| {
            unsafe {
                let current = *ptr.offset(i);
                if current == T::default() {
                    // Found first null, check for double null
                    if i + 1 < max_reasonable_size {
                        let next = *ptr.offset(i + 1);
                        // Continue if not double null
                        next != T::default()
                    } else {
                        // Stop if we're at the end
                        false
                    }
                } else {
                    // Continue if not null
                    true
                }
            }
        })
        .count();
    
    // Add 2 to include both null terminators if we found them
    if (size as isize) < max_reasonable_size {
        Some(size + 2)
    } else {
        None // Malformed or too large
    }
}
