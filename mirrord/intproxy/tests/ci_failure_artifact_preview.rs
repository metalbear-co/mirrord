#[cfg(target_os = "macos")]
#[test]
fn macos_failure_artifact_preview() {
    println!("macOS failure artifact stdout preview");
    eprintln!("macOS failure artifact stderr preview");
    panic!("intentional macOS failure for CI artifact validation");
}

#[cfg(target_os = "windows")]
#[test]
fn windows_failure_artifact_preview() {
    println!("Windows failure artifact stdout preview");
    eprintln!("Windows failure artifact stderr preview");
    panic!("intentional Windows failure for CI artifact validation");
}
