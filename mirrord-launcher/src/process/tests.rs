use super::*;

#[test]
fn dir_none() {
    let path = Path::new(r#"c:\windows"#);
    let file_name = process_name_from_path(path);

    assert_eq!(file_name, None);
}

#[test]
fn file_notepad() {
    let path = Path::new(r#"c:\windows\notepad.exe"#);
    let file_name = process_name_from_path(path);

    assert_eq!(file_name, Some("notepad.exe".to_string()));
}
