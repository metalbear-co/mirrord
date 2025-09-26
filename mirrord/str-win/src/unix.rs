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
    let mut result: Vec<u16> = string
        .as_ref()
        .chars()
        .map(|c| c as u16) // Simple conversion - not proper UTF-16
        .collect();
    result.push(0); // Add null terminator
    result
}
