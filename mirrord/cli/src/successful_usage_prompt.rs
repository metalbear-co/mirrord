use std::collections::HashMap;

use mirrord_progress::Progress;

use crate::user_data::UserData;

/// Link to the mirrord for Teams sign-up page (with UTM query params)
const TEAMS_SIGNUP_URL: &str =
    "https://app.metalbear.com/?utm_medium=cli&utm_source=successful_usage";

/// How many times mirrord can be run before suggesting mirrord for Teams the first time.
const PROMPT_AFTER_FIRST: u32 = 10;

/// How many times mirrord can be run before suggesting mirrord for Teams the second time.
const PROMPT_AFTER_SECOND: u32 = 50;

/// Suggests mirrord for Teams to users who have run mirrord successfully a certain number of
/// times without the operator. Skipped when the current run uses the operator.
pub async fn suggest_operator_features<P: Progress>(
    user_data: &UserData,
    uses_operator: bool,
    progress: &mut P,
) {
    if uses_operator {
        return;
    }

    let prompts = HashMap::from([
        (
            PROMPT_AFTER_FIRST,
            (
                "Working with mirrord in a team? With mirrord for Teams, multiple devs can target the same service without stepping on each other.",
                "prompt-1",
            ),
        ),
        (
            PROMPT_AFTER_SECOND,
            (
                "mirrord for Teams unlocks team workflow features: DB branching for parallel devs, preview environments for branch testing, and shared targets with queue splitting.",
                "prompt-2",
            ),
        ),
    ]);

    if let Some((message, content_tag)) = prompts.get(&user_data.session_count()) {
        progress.print(
            format!(
                "\n\n{}\n>> To try it, run:\n\
        >> mirrord teams\n\
        >> or visit: {TEAMS_SIGNUP_URL}&utm_content={}\n",
                message, content_tag
            )
            .as_str(),
        );
    }
}
