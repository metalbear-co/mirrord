use std::io::{self, IsTerminal};

use mirrord_progress::Progress;

/// Set to `true` to never show the [`OPERATOR_TIP`].
pub(crate) const MIRRORD_DISABLE_OPERATOR_TIP_ENV: &str = "MIRRORD_DISABLE_OPERATOR_TIP";

/// Run counts on which runs without the operator show the [`OPERATOR_TIP`].
const OPERATOR_TIP_RUNS: [u32; 4] = [3, 25, 57, 75];

/// Points runs without the operator to its trial and free tier.
const OPERATOR_TIP: &str = ">> Try queue splitting and DB branching free for 7 days, then stay on \
the free tier: mirrord operator install";

/// Suggests `mirrord operator install` on certain run counts of runs without the operator.
///
/// The tip is printed after the `progress` task succeeds, and only in an interactive terminal
/// outside of CI. IDEs use the JSON progress, which does not print it.
pub(crate) fn suggest_operator_install<P: Progress>(
    run_count: u32,
    uses_operator: bool,
    mirrord_for_ci: bool,
    progress: &mut P,
) {
    let disabled = std::env::var(MIRRORD_DISABLE_OPERATOR_TIP_ENV)
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .and_then(|value| value.parse::<bool>().ok())
        .unwrap_or_default();

    if OPERATOR_TIP_RUNS.contains(&run_count)
        && !uses_operator
        && !mirrord_for_ci
        && !ci_info::is_ci()
        && io::stdout().is_terminal()
        && !disabled
    {
        progress.add_to_print_buffer(OPERATOR_TIP);
    }
}
