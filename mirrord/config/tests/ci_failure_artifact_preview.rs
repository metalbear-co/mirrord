#[test]
fn integration_failure_artifact_preview() {
    println!("integration failure artifact stdout preview");
    eprintln!("integration failure artifact stderr preview");
    panic!("intentional integration failure for CI artifact validation");
}
