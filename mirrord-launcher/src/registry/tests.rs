use rand::{Rng, distr::Alphanumeric};

use super::*;

fn random_string() -> String {
    let s: String = rand::rng()
        .sample_iter(&Alphanumeric)
        .take(16)
        .map(char::from)
        .collect();
    s
}

#[test]
fn open_hkcu_classes() {
    let hkcu = Registry::hkcu();
    let classes = hkcu.get_key(r#"Software\Classes\"#);
    assert!(classes.is_some());
}

#[test]
fn open_hkcu_random_inexistent() {
    let hkcu = Registry::hkcu();
    let classes = hkcu.get_key(r#"Software\Not Existent"#);
    assert!(classes.is_none());
}

#[test]
fn create_hkcu_key_and_delete() {
    let mut root = Registry::hkcu().get_key("Software").unwrap();
    let random = random_string();

    let random_key = root.get_key(&random);
    assert!(random_key.is_none());

    let random_key = root.insert_key(&random);
    assert!(random_key.is_some());

    root.delete_key(&random);

    let random_key = root.get_key(&random);
    assert!(random_key.is_none());
}

#[test]
fn hkcu_get_keys() {
    let hkcu = Registry::hkcu();
    let keys = hkcu.keys();

    let software = keys.get("Software");
    assert!(software.is_some());

    let classes = software.unwrap().get_key("Classes");
    assert!(classes.is_some());

    let png = classes.unwrap().get_key(".png");
    assert!(png.is_some());
}

#[test]
fn hkcu_get_dwm_accent_color() {
    let hkcu = Registry::hkcu();

    let dwm = hkcu.get_key(r#"Software\Microsoft\Windows\DWM"#);
    assert!(dwm.is_some());

    let dwm = dwm.unwrap();
    let accent_color = dwm.get_value_dword("AccentColor");
    assert!(accent_color.is_some());
}

#[test]
fn hkcu_get_preferred_languages() {
    let hkcu = Registry::hkcu();

    let mui_cached = hkcu.get_key(r#"Control Panel\Desktop\MuiCached"#);
    assert!(mui_cached.is_some());

    let mui_cached = mui_cached.unwrap();
    let preferred_languages = mui_cached.get_value_multi_string("MachinePreferredUILanguages");
    assert!(preferred_languages.is_some());

    let preferred_languages = preferred_languages.unwrap();
    assert!(!preferred_languages.is_empty());

    let first = preferred_languages.first();
    assert_eq!(first, Some(&"en-US".to_string()));
}

#[test]
fn hkcu_set_get_value() {
    let mut hkcu = Registry::hkcu();

    let mut key_name = r#"Software\"#.to_string();
    key_name.push_str(&random_string());
    let value_name = random_string();

    let key = hkcu.insert_key(&key_name);
    assert!(key.is_some());

    let mut key = key.unwrap();
    let inserted = key.insert_value_string(&value_name, "Hello, world!".to_string());
    assert!(inserted);

    let value = key.get_value_string(value_name);
    assert_eq!(value, Some("Hello, world!".to_string()));

    let deleted = hkcu.delete_key(key_name);
    assert!(deleted);
}

#[test]
fn hkcu_set_get_values() {
    let mut hkcu = Registry::hkcu();

    let mut key_name = r#"Software\"#.to_string();
    key_name.push_str(&random_string());

    let key = hkcu.insert_key(&key_name);
    assert!(key.is_some());

    let mut key = key.unwrap();

    let inserted = key.insert_value_binary("Binary", vec![b'H', b'i', b'!']);
    assert!(inserted);

    let inserted = key.insert_value_dword("Dword", 123);
    assert!(inserted);

    let inserted = key.insert_value_qword("Qword", 456);
    assert!(inserted);

    let inserted = key.insert_value_string("String", "Hello!".to_string());
    assert!(inserted);

    let inserted = key.insert_value_multi_string(
        "MultiString",
        vec!["Hello".to_string(), "world!".to_string()],
    );
    assert!(inserted);

    // --------------------------------

    let binary = key.get_value_binary("Binary");
    assert_eq!(binary, Some(vec![b'H', b'i', b'!']));

    let dword = key.get_value_dword("Dword");
    assert_eq!(dword, Some(123));

    let qword = key.get_value_qword("Qword");
    assert_eq!(qword, Some(456));

    let test_string = key.get_value_string("String");
    assert_eq!(test_string, Some("Hello!".to_string()));

    let test_multi_string = key.get_value_multi_string("MultiString");
    assert_eq!(
        test_multi_string,
        Some(vec!["Hello".to_string(), "world!".to_string()])
    );

    let values = key.values();
    assert!(values.contains_key(&"Binary".to_string()));
    assert!(values.contains_key(&"Dword".to_string()));
    assert!(values.contains_key(&"Qword".to_string()));
    assert!(values.contains_key(&"String".to_string()));
    assert!(values.contains_key(&"MultiString".to_string()));

    let deleted = hkcu.delete_key(key_name);
    assert!(deleted);
}

#[test]
fn test_string_to_multi_string_buffer() {
    let strings: Vec<String> = vec!["ab".to_owned()];
    let multi_buffer = strings_to_multi_string_buffer(&strings);
    assert!(!multi_buffer.is_empty());

    let strings_from_multi_buffer = u16_multi_buffer_to_strings(multi_buffer);
    assert_eq!(strings_from_multi_buffer, strings);
}

#[test]
fn test_strings_to_multi_string_buffer() {
    let strings: Vec<String> = vec![
        "ab".to_owned(),
        "cd".to_owned(),
        "e".to_owned(),
        "f".to_owned(),
    ];
    let multi_buffer = strings_to_multi_string_buffer(&strings);
    assert!(!multi_buffer.is_empty());

    let strings_from_multi_buffer = u16_multi_buffer_to_strings(multi_buffer);
    assert_eq!(strings_from_multi_buffer, strings);
}
