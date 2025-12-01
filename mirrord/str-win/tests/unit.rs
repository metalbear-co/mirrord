use str_win::*;

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

    let strings = multi_buffer_to_strings(&example_wide);
    assert!(!strings.is_empty());

    assert_eq!(strings, ["String1", "String2", "String3", "LastString"]);

    let example_ansi: &[u8] = b"String1\0String2\0String3\0LastString\0\0";

    let strings = multi_buffer_to_strings(example_ansi);
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

    let strings = multi_buffer_to_strings(env_buffer);
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

    let strings = multi_buffer_to_strings(&malformed_buffer);
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

    let new_path = path_to_unix_path(WINDOWS_PATH);
    assert!(&new_path.is_some());
    assert_eq!(Path::new(&new_path.unwrap()), Path::new(UNIX_PATH));
}

#[test]
fn try_all_possible_volume_letters_to_unix_path() {
    for c in 'C'..'Z' {
        let path = format!("\\??\\{c}:\\windows\\system32");

        assert_eq!(
            path_to_unix_path(path),
            Some("/windows/system32".to_owned())
        );
    }
}

#[test]
fn try_just_all_possible_volume_letters_to_unix_path() {
    for c in 'C'..'Z' {
        let path = format!("\\??\\{c}:\\");

        assert_eq!(path_to_unix_path(path), Some("/".to_owned()));
    }
}
