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
    buffer.iter().position(|x| *x == null_value).unwrap_or(buffer.len())
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

    while start < buffer.len() {
        // Find the length of current string using utility function
        let len = if start < buffer.len() {
            find_null_terminator_length(&buffer[start..], T::default())
        } else {
            0
        };

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
    while pos < slice.len() {
        let remaining_slice = &slice[pos..];
        let next_null = find_null_terminator_length(remaining_slice, T::default());
        
        if next_null == 0 {
            // Found a null at current position, check if next is also null (double null)
            if pos + 1 < slice.len() && slice[pos + 1] == T::default() {
                // Found double null terminator
                return Some(pos + 2);
            }
            // Single null, move past it
            pos += 1;
        } else {
            // Move past this string and its null terminator
            pos += next_null + 1;
        }
    }
    
    None // Double null terminator not found within bounds
}
