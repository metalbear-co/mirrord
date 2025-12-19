use std::collections::HashMap;

use mirrord_progress::Progress;
use tracing::trace;

use crate::user_data::UserData;

/// Link to the mirrord newsletter signup page (with UTM query params)
const NEWSLETTER_SIGNUP_URL: &str =
    "https://metalbear.com/newsletter?utm_medium=cli&utm_source=newsletter";

/// How many times mirrord can be run before inviting the user to sign up to the newsletter the
/// first time.
const NEWSLETTER_COUNTER_PROMPT_AFTER_FIRST: u32 = 5;

/// How many times mirrord can be run before inviting the user to sign up to the newsletter the
/// second time.
const NEWSLETTER_COUNTER_PROMPT_AFTER_SECOND: u32 = 20;

/// How many times mirrord can be run before inviting the user to sign up to the newsletter the
/// third time.
const NEWSLETTER_COUNTER_PROMPT_AFTER_THIRD: u32 = 100;

/// Called during normal execution, suggests newsletter signup if the user has run mirrord a certain
/// number of times.
pub async fn suggest_newsletter_signup<P: Progress>(user_data: &mut UserData, progress: &mut P) {
    let newsletter_invites = HashMap::from([
        (
            NEWSLETTER_COUNTER_PROMPT_AFTER_FIRST,
            "Join thousands of devs using mirrord!".to_string(),
        ),
        (
            NEWSLETTER_COUNTER_PROMPT_AFTER_SECOND,
            "Liking what mirrord can do?".to_string(),
        ),
        (
            NEWSLETTER_COUNTER_PROMPT_AFTER_THIRD,
            "Looks like you're doing some serious work with mirrord!".to_string(),
        ),
    ]);

    let current_sessions = user_data
        .bump_session_count()
        .await
        .inspect_err(|fail| {
            // in case of failure to update, return the count as zero to prevent any prompts from
            // being shown repeatedly if the update fails multiple times
            trace!(
                %fail,
                "Failed to update number of previous mirrord runs."
            );
        })
        .unwrap_or_default();

    if let Some(message) = newsletter_invites.get(&current_sessions) {
        // print the chosen invite to the user if progress mode is on
        progress.add_to_print_buffer(
            format!(
                "\n\n{}\n>> To subscribe to the mirrord newsletter, run:\n\
        >> mirrord newsletter\n\
        >> or sign up here: {NEWSLETTER_SIGNUP_URL}{}\n",
                message, current_sessions
            )
            .as_str(),
        );
    }
}

/// Attempts to open the mirrord newsletter sign-up page in the default browser.
/// In case of failure, prints the link.
pub async fn newsletter_command() {
    // open URL with param utm_source=newslettercmd
    let url = format!("{NEWSLETTER_SIGNUP_URL}cmd");
    if let Err(error) = opener::open(&url) {
        tracing::trace!("failed to open browser, command result: {error:?}");
        println!(
            "To sign up for the mirrord newsletter and get notified of new features as they come out, visit:\n\n\
             {url}"
        );
    }
}
