use super::*;

#[test]
fn multi_string() {
    let example_wide: Vec<u16> = b"String1\0String2\0String3\0LastString\0\0"
        .iter()
        .map(|&x| u16::from(x))
        .collect();

    let strings = u16_multi_buffer_to_strings(example_wide);
    assert!(!strings.is_empty());

    assert_eq!(strings, ["String1", "String2", "String3", "LastString"]);
}

#[test]
fn string_to_u8() {
    let str = "abcd".to_owned();

    let bytes = string_to_u8_buffer(str);
    assert_eq!(bytes.len(), 5);
    assert_eq!(
        bytes.as_slice(),
        &['a' as u8, 'b' as u8, 'c' as u8, 'd' as u8, 0u8]
    );
}
