/// Attempts to open mirrord for Teams introduction in the default browser.
/// In case of failure, prints the link.
pub async fn navigate_to_intro() {
    const MIRRORD_FOR_TEAMS_URL: &str =
        "https://metalbear.com/mirrord/docs/overview/teams/?utm_source=teamscmd&utm_medium=cli";

    if let Err(error) = opener::open(MIRRORD_FOR_TEAMS_URL) {
        tracing::trace!("failed to open browser, command result: {error:?}");
        println!("To try mirrord for Teams, visit {MIRRORD_FOR_TEAMS_URL}");
    }
}
