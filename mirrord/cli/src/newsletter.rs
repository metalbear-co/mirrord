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
    let newsletter_invites = vec![
        NewsletterPrompt {
            runs: NEWSLETTER_INVITE_FIRST,
            message: format!("Nice! You ran {NEWSLETTER_INVITE_FIRST} successful mirrord sessions."),
        },
        NewsletterPrompt {
            runs: NEWSLETTER_INVITE_SECOND,
            message: "Liking what mirrord can do?".to_string(),
        },NewsletterPrompt {
            runs: NEWSLETTER_INVITE_THIRD,
            message: format!("{NEWSLETTER_INVITE_THIRD} sessions with mirrord! Looks like you're doing some serious work"),
        }
    ];

    let current_sessions = bump_session_count().await;
    let invite: Vec<_> = newsletter_invites
        .iter()
        .filter_map(|invite| {
            if invite.runs == current_sessions {
                Some(invite)
            } else {
                None
            }
        })
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
pub async fn bump_session_count() -> u32 {
    let new_count = read_session_count().await + 1;
    // todo: set new_count into file
    new_count
}

pub async fn read_session_count() -> u32 {
    // todo: read from file
    20
}
