#[cfg(windows)]
use std::ptr;

use str_win::*;
#[cfg(windows)]
use winapi::{
    shared::minwindef::LPVOID,
    um::{
        memoryapi::{VirtualAlloc, VirtualFree, VirtualProtect},
        sysinfoapi::GetSystemInfo,
        winnt::{MEM_COMMIT, MEM_RELEASE, MEM_RESERVE, PAGE_NOACCESS, PAGE_READWRITE},
    },
};

#[test]
fn u8_to_str() {
    let buf = b"Hello!\0";
    let rust_str = u8_buffer_to_string(buf);
    assert_eq!(rust_str, "Hello!");

    let buf_rus = b"\xd0\xbf\xd1\x80\xd0\xb8\xd0\xb2\xd0\xb5\xd1\x82\0";
    let rust_str = u8_buffer_to_string(buf_rus);
    assert_eq!(rust_str, "привет");
}

#[test]
fn u16_to_str() {
    let buf: Vec<u16> = b"Hello!\0".iter().copied().map(u16::from).collect();
    let rust_str = u16_buffer_to_string(buf);
    assert_eq!(rust_str, "Hello!");
}

#[test]
fn multi_string() {
    let example_wide: Vec<u16> = b"String1\0String2\0String3\0LastString\0\0"
        .iter()
        .map(|&x| u16::from(x))
        .collect();

    let strings = unsafe { multi_buffer_ptr_to_strings(example_wide.as_ptr()) };
    assert!(!strings.is_empty());

    assert_eq!(strings, ["String1", "String2", "String3", "LastString"]);

    let example_ansi: &[u8] = b"String1\0String2\0String3\0LastString\0\0";

    let strings = unsafe { multi_buffer_ptr_to_strings(example_ansi.as_ptr()) };
    assert!(!strings.is_empty());

    assert_eq!(strings, ["String1", "String2", "String3", "LastString"]);
}

#[test]
fn string_to_u8() {
    let str = "abcd".to_owned();

    let bytes = string_to_u8_buffer(str);
    assert_eq!(bytes.len(), 5);
    assert_eq!(bytes.as_slice(), &[b'a', b'b', b'c', b'd', 0u8]);
}

#[test]
fn u8_multi_buffer_environment_parsing() {
    // Test environment-like multi-string buffer with valid and invalid entries
    let env_buffer: &[u8] = b"PATH=/usr/bin\0USER=test\0=INVALID\0NOEQUALS\0HOME=/home/user\0\0";

    let strings = unsafe { multi_buffer_ptr_to_strings(env_buffer.as_ptr()) };
    assert_eq!(strings.len(), 5); // Should include all entries, even invalid ones

    assert_eq!(
        strings,
        [
            "PATH=/usr/bin",
            "USER=test",
            "=INVALID", // Starts with = (invalid for env vars)
            "NOEQUALS", // No = sign (invalid for env vars)
            "HOME=/home/user"
        ]
    );
}

#[test]
fn u8_multi_buffer_malformed_utf8() {
    // Test buffer with invalid UTF-8 sequences
    let mut malformed_buffer = Vec::new();
    malformed_buffer.extend_from_slice(b"VALID=test");
    malformed_buffer.push(0);
    malformed_buffer.extend_from_slice(&[b'B', b'A', b'D', b'=', 0xFF, 0xFE]); // Invalid UTF-8
    malformed_buffer.push(0);
    malformed_buffer.push(0); // Double null terminator

    let strings = unsafe { multi_buffer_ptr_to_strings(malformed_buffer.as_ptr()) };
    assert_eq!(strings.len(), 2);

    assert_eq!(strings.first().map(String::as_str), Some("VALID=test"));
    // Second string should be handled with lossy conversion
    assert!(matches!(strings.get(1), Some(value) if value.starts_with("BAD=")));
}

#[test]
fn string_to_u16() {
    let str = "abcd".to_owned();

    let bytes = string_to_u16_buffer(str);
    assert_eq!(bytes.len(), 5);
    assert_eq!(bytes.as_slice(), &[97u16, 98u16, 99u16, 100u16, 0]);
}

#[test]
fn try_get_unix_path() {
    use std::path::Path;

    const WINDOWS_PATH: &str = r#"\??\C:\home\gabrielaelae\dev\MIRRORD\mirrord\target\debug"#;
    const UNIX_PATH: &str = r#"/home/gabrielaelae/dev/MIRRORD/mirrord/target/debug"#;

    let new_path = path_to_unix_path(WINDOWS_PATH).expect("an NT disk path converts");
    assert_eq!(Path::new(&new_path.path), Path::new(UNIX_PATH));
}

#[test]
fn try_all_possible_volume_letters_to_unix_path() {
    for c in 'C'..'Z' {
        let path = format!("\\??\\{c}:\\windows\\system32");

        assert_eq!(
            path_to_unix_path(path),
            Some(UnixPath {
                drive: Some(c),
                path: "/windows/system32".to_owned()
            })
        );
    }
}

#[test]
fn try_just_all_possible_volume_letters_to_unix_path() {
    for c in 'C'..'Z' {
        let path = format!("\\??\\{c}:\\");

        assert_eq!(
            path_to_unix_path(path),
            Some(UnixPath {
                drive: Some(c),
                path: "/".to_owned()
            })
        );
    }
}

/// Every spelling of a rooted path gives the drive-less form the fs patterns have always matched,
/// and the drive it names, if any.
#[cfg(windows)]
#[rstest::rstest]
#[case::nt_disk_path(
    r"\??\C:\Repos\app\appsettings.json",
    Some('C'),
    "/Repos/app/appsettings.json"
)]
#[case::win32_disk_path(r"D:\Repos\app.json", Some('D'), "/Repos/app.json")]
#[case::lowercase_drive_is_upper_cased(r"\??\d:\data", Some('D'), "/data")]
#[case::forward_slashes(r"E:/data/file.txt", Some('E'), "/data/file.txt")]
#[case::verbatim_disk_path(r"\\?\C:\Users\me", Some('C'), "/Users/me")]
#[case::drive_root_without_separator(r"\??\C:", Some('C'), "/")]
#[case::unc_path_has_no_drive(r"\\server\share\dir\file", None, "/dir/file")]
#[case::rooted_path_without_a_prefix(r"\Users\me", None, "/Users/me")]
fn rooted_paths_convert_with_their_drive(
    #[case] windows: &str,
    #[case] drive: Option<char>,
    #[case] unix: &str,
) {
    let converted = path_to_unix_path(windows).expect("a rooted path always converts");

    assert_eq!(converted.path, unix, "the drive-less form of {windows}");
    assert_eq!(converted.drive, drive, "the drive of {windows}");
}

/// A path with no root can't be placed on the remote, so it isn't converted.
#[cfg(windows)]
#[rstest::rstest]
#[case::relative(r"Repos\app.json")]
#[case::drive_relative(r"C:Repos\app.json")]
fn paths_without_a_root_do_not_convert(#[case] windows: &str) {
    assert_eq!(path_to_unix_path(windows), None);
}

/// The drive form is what a pattern naming a drive (`^C:/Repos/`) matches. A path with no drive
/// has no such form, so a drive pattern can't match a UNC path by accident.
#[cfg(windows)]
#[test]
fn with_drive_puts_the_drive_before_the_unix_path() {
    let on_drive = path_to_unix_path(r"\??\C:\Repos\app.json").expect("an NT disk path converts");
    let on_share = path_to_unix_path(r"\\server\share\app.json").expect("a UNC path converts");

    assert_eq!(on_drive.with_drive().as_deref(), Some("C:/Repos/app.json"));
    assert_eq!(on_share.with_drive(), None);
}

/// Encodes `entries` as a Unicode block, then puts `after` behind it.
///
/// # Returns
///
/// The memory, and the length of the block in it.
fn block_then(entries: &[String], after: &[u16]) -> (Vec<u16>, usize) {
    let mut memory = entries
        .iter()
        .flat_map(|entry| entry.encode_utf16().chain([0]))
        .collect::<Vec<_>>();
    memory.push(0);
    let len = memory.len();
    memory.extend_from_slice(after);
    (memory, len)
}

/// A non-zero unit behind the block that a scan past its terminator would run into.
const NOT_A_TERMINATOR: u16 = 0x41;

#[test]
fn an_empty_block_has_no_strings() {
    assert!(unsafe { multi_buffer_ptr_to_strings([0u16, 0].as_ptr()) }.is_empty());
    assert!(unsafe { multi_buffer_ptr_to_strings([0u16, NOT_A_TERMINATOR].as_ptr()) }.is_empty());
    assert!(unsafe { multi_buffer_ptr_to_strings([0u8, 0].as_ptr()) }.is_empty());
}

#[test]
fn a_single_entry_ends_at_the_block_terminator() {
    let (memory, _) = block_then(&["A=1".to_owned()], &[NOT_A_TERMINATOR, 0, 0]);
    assert_eq!(
        unsafe { multi_buffer_ptr_to_strings(memory.as_ptr()) },
        ["A=1"]
    );

    let ansi = [b'A', b'=', b'1', 0, 0, b'A', 0, 0];
    assert_eq!(
        unsafe { multi_buffer_ptr_to_strings(ansi.as_ptr()) },
        ["A=1"]
    );
}

/// Whatever follows the block, zero or not, is not part of it.
#[test]
fn the_scan_stops_at_the_block_terminator() {
    let entries = ["A=1".to_owned(), "=C:=C:/work".to_owned(), "B=".to_owned()];
    for after in [[0, 0], [NOT_A_TERMINATOR, 0], [0, NOT_A_TERMINATOR]] {
        let (memory, _) = block_then(&entries, &after);
        assert_eq!(
            unsafe { multi_buffer_ptr_to_strings(memory.as_ptr()) },
            entries,
            "{after:?}"
        );
    }
}

/// A pod's worth of service variables: well over 32768 units as a block.
fn service_entries() -> Vec<String> {
    (0..800)
        .map(|i| {
            format!(
                "SERVICE_{i:04}_PORT_8080_TCP_ADDR=10.96.{}.{}",
                i / 250,
                i % 250
            )
        })
        .collect()
}

/// Windows limits one variable, not the block: a pod's environment easily passes 32768 units.
#[test]
fn a_block_larger_than_32768_units_is_read_fully() {
    let entries = service_entries();
    let (memory, len) = block_then(&entries, &[NOT_A_TERMINATOR]);
    assert!(len > 32_768, "{len} units");

    assert_eq!(
        unsafe { multi_buffer_ptr_to_strings(memory.as_ptr()) },
        entries
    );
}

/// A copy of some units that ends where readable memory ends: the page after it is inaccessible,
/// so reading one unit past the copy ends the test process with an access violation.
#[cfg(windows)]
struct Guarded {
    allocation: LPVOID,
    start: *mut u8,
}

#[cfg(windows)]
impl Guarded {
    fn new<T: Copy>(units: &[T]) -> Self {
        let page = unsafe {
            let mut info = std::mem::zeroed();
            GetSystemInfo(&mut info);
            info.dwPageSize as usize
        };
        let bytes = std::mem::size_of_val(units);
        let pages = bytes.div_ceil(page);
        unsafe {
            let allocation = VirtualAlloc(
                ptr::null_mut(),
                (pages + 1) * page,
                MEM_RESERVE | MEM_COMMIT,
                PAGE_READWRITE,
            );
            assert!(!allocation.is_null(), "allocate the pages");
            let guard = allocation.cast::<u8>().add(pages * page);
            let mut previous = 0;
            assert_ne!(
                VirtualProtect(guard.cast(), page, PAGE_NOACCESS, &mut previous),
                0,
                "make the page after the copy inaccessible"
            );
            let start = guard.sub(bytes);
            ptr::copy_nonoverlapping(units.as_ptr().cast::<u8>(), start, bytes);
            Self { allocation, start }
        }
    }

    fn ptr<T>(&self) -> *const T {
        self.start.cast_const().cast()
    }
}

#[cfg(windows)]
impl Drop for Guarded {
    fn drop(&mut self) {
        unsafe { VirtualFree(self.allocation, 0, MEM_RELEASE) };
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

fn narrow(text: &str) -> Vec<u8> {
    text.bytes().chain([0]).collect()
}

/// A string is read up to its NUL, and not one unit further.
#[cfg(windows)]
#[test]
fn a_string_is_read_only_up_to_its_nul() {
    let library = Guarded::new(&wide("kernel32.dll"));
    assert_eq!(unsafe { u16_ptr_to_string(library.ptr()) }, "kernel32.dll");
    assert_eq!(
        unsafe { u16_ptr_to_string(Guarded::new(&[0u16]).ptr()) },
        ""
    );

    let function = Guarded::new(&narrow("GetProcAddress"));
    assert_eq!(
        unsafe { u8_ptr_to_string(function.ptr()) },
        "GetProcAddress"
    );
    assert_eq!(unsafe { u8_ptr_to_string(Guarded::new(&[0u8]).ptr()) }, "");
}

/// A block is read up to the empty string that ends it, and not one unit further.
#[cfg(windows)]
#[test]
fn a_block_is_read_only_up_to_its_terminator() {
    let entries = ["A=1".to_owned(), "PATH=C:/Windows".to_owned()];
    let (block, _) = block_then(&entries, &[]);
    assert_eq!(
        unsafe { multi_buffer_ptr_to_strings(Guarded::new(&block).ptr::<u16>()) },
        entries
    );
    assert!(unsafe { multi_buffer_ptr_to_strings(Guarded::new(&[0u16]).ptr::<u16>()) }.is_empty());

    assert_eq!(
        unsafe { multi_buffer_ptr_to_strings(Guarded::new(b"A=1\0\0").ptr::<u8>()) },
        ["A=1"]
    );
    assert!(unsafe { multi_buffer_ptr_to_strings(Guarded::new(&[0u8]).ptr::<u8>()) }.is_empty());
}

/// A block larger than 32768 units is read whole, and not one unit past its terminator.
#[cfg(windows)]
#[test]
fn a_large_block_is_read_only_up_to_its_terminator() {
    let (block, len) = block_then(&service_entries(), &[]);
    assert!(len > 32_768, "{len} units");
    assert_eq!(
        unsafe { multi_buffer_ptr_to_strings(Guarded::new(&block).ptr::<u16>()) },
        service_entries()
    );
}

/// No length is assumed: a string longer than 32768 units comes back whole.
#[test]
fn a_string_longer_than_32768_units_is_not_truncated() {
    let text = "a".repeat(40_000);
    assert_eq!(unsafe { u16_ptr_to_string(wide(&text).as_ptr()) }, text);
    assert_eq!(
        unsafe { u8_ptr_to_string(narrow(&text).as_ptr().cast()) },
        text
    );
}

#[test]
fn a_null_pointer_is_an_empty_string() {
    assert_eq!(unsafe { u16_ptr_to_string(std::ptr::null()) }, "");
    assert_eq!(unsafe { u8_ptr_to_string(std::ptr::null()) }, "");
}

/// Text that is not valid in its encoding is kept, with replacement characters where it breaks.
#[test]
fn invalid_text_is_replaced_not_dropped() {
    let narrow = [b'a', 0xFF, b'b', 0];
    assert_eq!(
        unsafe { u8_ptr_to_string(narrow.as_ptr().cast()) },
        "a\u{FFFD}b"
    );
    let wide = [u16::from(b'a'), 0xD800, u16::from(b'b'), 0];
    assert_eq!(unsafe { u16_ptr_to_string(wide.as_ptr()) }, "a\u{FFFD}b");
}
