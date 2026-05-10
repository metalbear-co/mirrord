//! Heuristic for whether the current `mirrord exec` run is interactive (a human at a terminal)
//! or automated (CI pipeline, AI coding agent, piped output). Used to suppress marketing
//! prompts in non-interactive contexts.

use std::io::IsTerminal;

const NON_INTERACTIVE_ENV_VARS: &[&str] = &[
    "CI",
    "GITHUB_ACTIONS",
    "CIRCLECI",
    "GITLAB_CI",
    "BUILDKITE",
    "JENKINS_URL",
    "TRAVIS",
    "TF_BUILD",
    "CLAUDECODE",
    "CURSOR_AGENT",
    "AIDER_CHAT",
];

/// Returns `true` if the current mirrord run looks like an interactive human session.
pub fn is_interactive_session() -> bool {
    if let Ok(mode) = std::env::var("MIRRORD_PROGRESS_MODE") {
        let mode = mode.to_lowercase();
        if mode == "json" || mode == "off" {
            return false;
        }
    }

    if !std::io::stderr().is_terminal() {
        return false;
    }

    for var in NON_INTERACTIVE_ENV_VARS {
        if std::env::var(var).is_ok() {
            return false;
        }
    }

    true
}
