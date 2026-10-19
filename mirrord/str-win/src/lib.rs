use std::{
    ffi::CStr,
    path::{Component, Path, PathBuf, Prefix},
};

// This prefix is a way to explicitly indicate that we're looking in
// the global namespace for a path.
pub const GLOBAL_NAMESPACE_PATH: &str = r#"\??\"#;

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

    // Proper UTF-16 decoding: surrogate pairs become a single character, and an unpaired surrogate
    // becomes the replacement character rather than being dropped.
    String::from_utf16_lossy(buffer.get(..len).unwrap_or(buffer))
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
        // Proper UTF-16: pair surrogates instead of dropping them (`char::from_u32` returns `None`
        // for a lone surrogate, which silently lost astral characters).
        String::from_utf16_lossy(slice)
    }
}

pub fn string_to_u8_buffer<T: AsRef<str>>(string: T) -> Vec<u8> {
    let mut bytes = string.as_ref().as_bytes().to_vec();
    bytes.push(0); // Add null terminator
    bytes
}

/// Converts a string to a null-terminated UTF-16 buffer.
///
/// This is proper UTF-16: characters outside the basic multilingual plane become surrogate pairs
/// rather than being truncated to a single `u16`.
pub fn string_to_u16_buffer<T: AsRef<str>>(string: T) -> Vec<u16> {
    string.as_ref().encode_utf16().chain(Some(0)).collect()
}

/// Counts the units of a NUL-terminated string, without its NUL.
///
/// The string is read one unit at a time, up to and including its NUL and never past it, so a
/// string that ends at the edge of readable memory is safe to measure, and a long one is measured
/// whole.
///
/// # Safety
///
/// `ptr` must point to a readable, properly aligned string that ends with a NUL.
unsafe fn nul_terminated_len<T: Copy + PartialEq + Default>(ptr: *const T) -> usize {
    let mut len = 0;
    // SAFETY: every unit up to the string's NUL is readable, and the loop stops there.
    while unsafe { ptr.add(len).read() } != T::default() {
        len += 1;
    }
    len
}

/// Convert a null-terminated C string pointer to a Rust String.
///
/// Invalid UTF-8 becomes the replacement character.
///
/// # Safety
///
/// `ptr` must be null or point to a readable null-terminated C string. Nothing past its null
/// terminator is read.
///
/// # Returns
///
/// The converted text, or an empty string if the pointer is null.
pub unsafe fn u8_ptr_to_string(ptr: *const i8) -> String {
    if ptr.is_null() {
        return String::new();
    }

    unsafe { CStr::from_ptr(ptr.cast()) }
        .to_string_lossy()
        .into_owned()
}

/// Convert a null-terminated wide string pointer to a Rust String.
///
/// An unpaired surrogate becomes the replacement character.
///
/// # Safety
///
/// `ptr` must be null or point to a readable, properly aligned null-terminated wide string.
/// Nothing past its null terminator is read.
///
/// # Returns
///
/// The converted text, or an empty string if the pointer is null.
pub unsafe fn u16_ptr_to_string(ptr: *const u16) -> String {
    if ptr.is_null() {
        return String::new();
    }

    let len = unsafe { nul_terminated_len(ptr) };
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// The strings of a multi-string block (`MULTI_SZ`, such as an environment block): NUL-terminated
/// strings, ended by an empty string.
///
/// The block is read one string at a time, from its start up to and including the NUL of that
/// empty string, and never past it, so a block that ends at the edge of readable memory is safe to
/// read. There is no size limit: Windows limits one environment variable to 32767 characters, but
/// not the block that holds them.
///
/// # Safety
///
/// `ptr` must point to a readable, properly aligned block that is ended by an empty string.
pub unsafe fn multi_buffer_ptr_to_strings<T: MultiBufferChar>(mut ptr: *const T) -> Vec<String> {
    let mut strings = Vec::new();
    loop {
        let len = unsafe { nul_terminated_len(ptr) };
        if len == 0 {
            return strings;
        }
        strings.push(T::slice_to_string(unsafe {
            std::slice::from_raw_parts(ptr, len)
        }));
        // SAFETY: the string's NUL is readable, so the unit after it is the next string's start
        // or the block's terminator.
        ptr = unsafe { ptr.add(len + 1) };
    }
}

/// A Windows path in the Unix form the layer matches `feature.fs` patterns against and sends to the
/// agent, together with the drive it was on.
///
/// The agent sees only [`UnixPath::path`], since the remote pod has no drives. The drive is kept so
/// a pattern can name it: [`UnixPath::with_drive`] is the form a pattern like `^D:/data/` matches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnixPath {
    /// The drive letter, in upper case, for a path on a drive (`C:\Users`, `\\?\C:\Users`).
    /// [`None`] for a path with no drive, such as a UNC path or `\Users`.
    pub drive: Option<char>,
    /// The path without its drive, with forward slashes: `C:\Users\me` becomes `/Users/me`.
    pub path: String,
}

impl UnixPath {
    /// The path with its drive, with forward slashes: `C:\Users\me` becomes `C:/Users/me`.
    /// [`None`] when the path has no drive.
    pub fn with_drive(&self) -> Option<String> {
        self.drive
            .map(|drive| format!("{drive}:{path}", path = self.path))
    }
}

/// Turns a rooted Windows path, an NT one (`\??\C:\Users`) included, into its [`UnixPath`].
///
/// Every rooted path converts, so this does not check that the path is on a disk. The file hooks
/// do that first, with `is_nt_path_disk_path`.
///
/// ## Implementation
///
/// 1. Return [`None`] for a path with no root (`C:Users`, `Users`).
/// 2. Remove the NT global namespace prefix (`\??\`), which Rust doesn't parse as a prefix.
/// 3. Remove the path's prefix, if it has one. A drive prefix (`C:`, `\\?\C:`) becomes
///    [`UnixPath::drive`]; any other prefix (UNC, device) is dropped.
/// 4. Make sure what's left starts with a separator, so the root of a drive is `/`.
/// 5. Turn backslashes into forward slashes.
///
/// # Arguments
///
/// * `path` - A rooted Windows path.
pub fn path_to_unix_path<T: AsRef<Path>>(path: T) -> Option<UnixPath> {
    let mut path = path.as_ref();

    if !path.has_root() {
        return None;
    }

    // Rust doesn't know how to separate the components in this case.
    if path.starts_with(GLOBAL_NAMESPACE_PATH) {
        path = path.strip_prefix(GLOBAL_NAMESPACE_PATH).ok()?;
    }

    let mut components = path.components().peekable();
    let drive = match components.peek() {
        Some(Component::Prefix(prefix)) => {
            let drive = match prefix.kind() {
                Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => {
                    Some(char::from(letter).to_ascii_uppercase())
                }
                _ => None,
            };
            components.next();
            drive
        }
        _ => None,
    };
    let mut new_path: PathBuf = components.collect();

    // NOTE(gabriela): WIN-56 agent `strip_prefix``
    // If need be, implement RootDir component so that agent doesn't blow up.
    if !new_path.starts_with("/") {
        new_path = PathBuf::from(&"/").join(new_path);
    }

    // Turn to string, replace Windows slashes to Linux slashes for ease of use.
    Some(UnixPath {
        drive,
        path: new_path.to_str()?.replace('\\', "/"),
    })
}
