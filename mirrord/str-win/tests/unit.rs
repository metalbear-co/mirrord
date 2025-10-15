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

    let strings = u16_multi_buffer_to_strings(example_wide);
    assert!(!strings.is_empty());

    assert_eq!(strings, ["String1", "String2", "String3", "LastString"]);

    let example_ansi: &[u8] = b"String1\0String2\0String3\0LastString\0\0";

    let strings = u8_multi_buffer_to_strings(example_ansi);
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
fn string_to_u16() {
    let str = "abcd".to_owned();

    let bytes = string_to_u16_buffer(str);
    assert_eq!(bytes.len(), 5);
    assert_eq!(bytes.as_slice(), &[97u16, 98u16, 99u16, 100u16, 0]);
}
