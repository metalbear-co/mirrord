use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::{OsStrExt, OsStringExt};

pub fn u16_buffer_to_string<T: AsRef<[u16]>>(buffer: T) -> String {
    let buffer = buffer.as_ref();

    let len = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    OsString::from_wide(&buffer[..len])
        .to_string_lossy()
        .into_owned()
}

pub fn u16_multi_buffer_to_strings<T: AsRef<[u16]>>(buffer: T) -> Vec<String> {
    let buffer = buffer.as_ref();

    let mut strings: Vec<&[u16]> = buffer.split(|&c| c == 0).collect();
    let strings_len = strings.len();

    if strings.is_empty() {
        return vec![];
    }

    // If the last zero is not the terminator of the MULTI_SZ.
    if !strings.last().unwrap().is_empty() {
        return vec![];
    }

    // Eliminate terminator.
    strings.pop();

    // If there's only one string, there's no null-termiantor for the list,
    // only for the string.
    if strings_len > 2 {
        // If the last zero is not between the terminator of the MULTI_SZ.
        if !strings.last().unwrap().is_empty() {
            return vec![];
        }

        // Eliminate in-between.
        strings.pop();
    }

    // Check to see if there isn't any delimitator before.
    if strings.iter().any(|&x| x.is_empty()) {
        return vec![];
    }

    let strings: Vec<String> = strings.iter().map(|&x| u16_buffer_to_string(x)).collect();

    strings
}

pub fn string_to_u16_buffer<T: AsRef<str>>(string: T) -> Vec<u16> {
    OsStr::new(string.as_ref())
        .encode_wide()
        .chain(Some(0))
        .collect()
}

#[cfg(test)]
mod tests;
