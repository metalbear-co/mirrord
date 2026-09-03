#[cfg(feature = "cli")]
#[test]
fn cli_failure_artifact_preview() {
    fail("CLI");
}

#[cfg(feature = "targetless")]
#[test]
fn targetless_failure_artifact_preview() {
    fail("targetless");
}

#[cfg(feature = "job")]
#[test]
fn job_failure_artifact_preview() {
    fail("job");
}

#[cfg(feature = "ephemeral")]
#[test]
fn ephemeral_failure_artifact_preview() {
    fail("ephemeral-agent");
}

#[cfg(feature = "ipv6")]
#[test]
fn ipv6_failure_artifact_preview() {
    fail("IPv6");
}

fn fail(group: &str) {
    println!("{group} E2E failure artifact stdout preview");
    eprintln!("{group} E2E failure artifact stderr preview");
    panic!("intentional {group} E2E failure for CI artifact validation");
}
