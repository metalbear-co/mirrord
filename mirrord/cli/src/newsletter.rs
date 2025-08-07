use std::collections::HashMap;

use mirrord_progress::MIRRORD_PROGRESS_ENV;
use tokio::process::Command;
use tracing::trace;

use crate::user_data::UserData;

/// Link to the mirrord newsletter signup page (with UTM query params)
const NEWSLETTER_SIGNUP_URL: &str =
    "https://metalbear.co/newsletter?utm_medium=cli&utm_source=newsletter";

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
pub async fn suggest_newsletter_signup(user_data: &mut UserData) {
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

    // FIXME: checking this env manually instead of calling a method on progress is a kludge,
    // unfortunately made necessary by the current state of `Progress`. This should be changed in
    // the future.
    match std::env::var(MIRRORD_PROGRESS_ENV).as_deref().ok() {
        None | Some("std") | Some("standard") => {
            if let Some(message) = newsletter_invites.get(&current_sessions) {
                // print the chosen invite to the user if progress mode is on
                println!(
                    "\n\n{}\n>> To subscribe to the mirrord newsletter, run:\n\
        >> mirrord newsletter\n\
        >> or sign up here: {NEWSLETTER_SIGNUP_URL}{}\n",
                    message, current_sessions
                );
            }
        }
        _ => {}
    }
}

#[cfg(target_os = "linux")]
fn get_open_command() -> Command {
    let mut command = Command::new("gio");
    command.arg("open");
    command
}

#[cfg(target_os = "macos")]
fn get_open_command() -> Command {
    Command::new("open")
}

/// WARNING: untested on `target_os = "windows"`, but if this fails the URL will get printed to the
/// terminal as a fallback.
#[cfg(target_os = "windows")]
fn get_open_command() -> Command {
    let mut command = Command::new("cmd.exe");
    command.arg("/C").arg("start").arg("");
    command
}

/// Attempts to open the mirrord newsletter sign-up page in the default browser.
/// In case of failure, prints the link.
pub async fn newsletter_command() {
    // open URL with param utm_source=newslettercmd
    let url = format!("{NEWSLETTER_SIGNUP_URL}cmd");
    match get_open_command().arg(&url).output().await {
        Ok(output) if output.status.success() => {}
        other => {
            tracing::trace!(?other, "failed to open browser");
            println!(
                "To sign up for the mirrord newsletter and get notified of new features as they come out, visit:\n\n\
             {url}"
            );
        }
    }
}
