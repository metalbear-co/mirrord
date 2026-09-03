#[test]
fn agent_failure_artifact_preview() {
    println!("agent failure artifact stdout preview");
    eprintln!("agent failure artifact stderr preview");
    panic!("intentional agent failure for CI artifact validation");
}
