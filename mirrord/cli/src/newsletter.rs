use std::{path::PathBuf, sync::LazyLock};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::{
    fs,
    io::{AsyncReadExt, AsyncWriteExt},
};
use tokio::process::Command;
use tracing::trace;

/// Link to the mirrord newsletter signup page (with UTM query params)
const NEWSLETTER_SIGNUP_URL: &str =
    "https://metalbear.co/newsletter?utm_medium=cli&utm_source=newslttr";

/// How many times mirrord can be run before inviting the user to sign up to the newsletter the
/// first time.
const NEWSLETTER_INVITE_FIRST: u32 = 5;

/// How many times mirrord can be run before inviting the user to sign up to the newsletter the
/// second time.
const NEWSLETTER_INVITE_SECOND: u32 = 20;

/// How many times mirrord can be run before inviting the user to sign up to the newsletter the
/// third time.
const NEWSLETTER_INVITE_THIRD: u32 = 100;

/// "~/.mirrord"
static DATA_STORE_DIR: LazyLock<PathBuf> = LazyLock::new(|| {
    home::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".mirrord")
});

/// "~/.mirrord/data.json"
static DATA_STORE_PATH: LazyLock<PathBuf> = LazyLock::new(|| DATA_STORE_DIR.join("data.json"));

struct NewsletterPrompt {
    pub runs: u32,
    message: String,
}

impl NewsletterPrompt {
    pub fn get_full_message(&self) -> String {
        format!(
            "\n\n{}\n>> To subscribe to the mirrord newsletter, run:\n\
        >> mirrord subscribe\n\
        >> or sign up here: {NEWSLETTER_SIGNUP_URL}{}\n",
            self.message, self.runs
        )
    }
}

/// Called during normal execution, suggests newsletter signup if the user has run mirrord a certain
/// number of times.
pub async fn suggest_newsletter_signup() {
    let newsletter_invites = [
        NewsletterPrompt {
            runs: NEWSLETTER_INVITE_FIRST,
            message: "Join thousands of devs using mirrord!".to_string(),
        },
        NewsletterPrompt {
            runs: NEWSLETTER_INVITE_SECOND,
            message: "Liking what mirrord can do?".to_string(),
        },NewsletterPrompt {
            runs: NEWSLETTER_INVITE_THIRD,
            message: "Looks like you're doing some serious work with mirrord!".to_string(),
        }
    ];

    let current_sessions = bump_session_count().await;
    let invite: Vec<_> = newsletter_invites
        .iter()
        .filter(|invite| invite.runs == current_sessions)
        .collect();
    let invite = match invite.len() {
        1 => invite
            .first()
            .expect("vec should not be empty if its length is one"),
        _ => {
            return;
        }
    };

    // print the chosen invite to the user
    println!("{}", invite.get_full_message());
}

/// Increases the session count by one and returns the number.
/// Accesses the count via a file in the global .mirrord dir
async fn bump_session_count() -> u32 {
    let user_data = UserData::from_default_path()
        .await
        .map_err(|error| {
            trace!(
                %error,
                "Failed to determine number of previous mirrord runs, defaulting to 0."
            )
        })
        .unwrap_or_default();
    let new_data = UserData {
        prev_runs: user_data.prev_runs + 1,
    };

    if let Err(error) = UserData::overwrite_to_file(&new_data).await {
        // in case of failure to update, return the count as zero to prevent any prompts from being
        // shown repeatedly if the update fails multiple times
        trace!(
            %error,
            "Failed to update number of previous mirrord runs."
        );
        return 0;
    }
    new_data.prev_runs
}

#[derive(Default, Debug, Serialize, Deserialize)]
struct UserData {
    prev_runs: u32,
}

impl UserData {
    /// Create `UserData` from the default file path (`DATA_STORE_PATH`)
    async fn from_default_path() -> Result<Self, DataStoreError> {
        if !DATA_STORE_DIR.exists() {
            fs::create_dir_all(&*DATA_STORE_DIR)
                .await
                .map_err(DataStoreError::ParentDir)?;
        }

        let mut store_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(DATA_STORE_PATH.as_path())
            .await
            .map_err(DataStoreError::FileAccess)?;

        let mut contents = vec![];
        store_file
            .read_to_end(&mut contents)
            .await
            .map_err(DataStoreError::FileAccess)?;
        let user_data: UserData =
            serde_json::from_slice(contents.as_slice()).map_err(DataStoreError::Json)?;
        Ok(user_data)
    }

    /// Overwrite the JSON contents at the default file path (`DATA_STORE_PATH`) with `UserData`
    async fn overwrite_to_file(&self) -> Result<(), DataStoreError> {
        // DATA_STORE_DIR and DATA_STORE_PATH are already known to exist
        let mut store_file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(DATA_STORE_PATH.as_path())
            .await
            .map_err(DataStoreError::FileAccess)?;

        let contents = serde_json::to_vec(&self).map_err(DataStoreError::Json)?;
        store_file
            .write_all(contents.as_slice())
            .await
            .map_err(DataStoreError::FileAccess)?;
        Ok(())
    }
}

/// Errors from [`CredentialStore`](crate::credential_store::CredentialStore) and
/// [`CredentialStoreSync`](crate::credential_store::CredentialStoreSync) operations
#[derive(Debug, Error)]
pub enum DataStoreError {
    #[error("failed to parent directory for data store file: {0}")]
    ParentDir(std::io::Error),

    #[error("IO on data store file failed: {0}")]
    FileAccess(std::io::Error),

    #[error("failed to serialize/deserialize data: {0}")]
    Json(serde_json::Error),
}

#[cfg(not(target_os = "macos"))]
fn get_open_command() -> Command {
    let mut command = Command::new("gio");
    command.arg("open");
    command
}

#[cfg(target_os = "macos")]
fn get_open_command() -> Command {
    Command::new("open")
}

/// Attempts to open the mirrord newsletter sign-up page in the default browser.
/// In case of failure, prints the link.
pub async fn newsletter_command() {
    // open URL with param utm_source=newslttrcmd
    let url = format!("{NEWSLETTER_SIGNUP_URL}cmd");
    match get_open_command().arg(&url).output().await {
        Ok(output) if output.status.success() => {}
        other => {
            tracing::trace!("failed to open browser, command result: {other:?}");
            println!("To sign up for the mirrord newsletter and get notified of new features as they come out, visit:\n\n\
             {url}");
        }
    }
}
