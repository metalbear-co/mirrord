//! A rotating tip printed at the end of `mirrord exec` output, pointing users at other
//! mirrord commands they may not know about.
//!
//! `mirrord exec` replaces the CLI process with the user's binary, so the tip is added to the
//! print buffer of the final progress task and flushes right after the "Ready!" line, making it
//! the last thing mirrord prints before the user's application takes over.

use console::Style;
use mirrord_progress::Progress;

use crate::user_data::UserData;

/// Commands advertised at the end of a `mirrord exec` run, as `(command, what it does)`.
const COMMAND_TIPS: &[(&str, &str)] = &[
    (
        "mirrord ui",
        "watch your mirrord sessions live in the browser",
    ),
    (
        "mirrord wizard",
        "build a mirrord config interactively instead of by hand",
    ),
    (
        "mirrord dump",
        "print a target's incoming traffic without running any app",
    ),
    (
        "mirrord port-forward",
        "reach hosts inside the cluster from local ports",
    ),
    (
        "mirrord diagnose latency",
        "measure the latency between your machine and the cluster",
    ),
];

/// ANSI 256-color for the tip line: an orange that stands out from the rest of the output,
/// in the style of tips printed by other developer CLIs.
const TIP_COLOR: u8 = 209;

/// Picks the tip for this run, rotating through [`COMMAND_TIPS`] with the session count so
/// consecutive runs surface different commands.
fn tip_for_session(session_count: u32) -> (&'static str, &'static str) {
    COMMAND_TIPS[session_count as usize % COMMAND_TIPS.len()]
}

/// Adds a one-line command tip to the progress print buffer, so it is printed as the last
/// mirrord line before the user's process starts.
///
/// [`Progress::add_to_print_buffer`] is a no-op outside of the interactive spinner mode, so
/// IDE (json) and simple progress outputs are unaffected. [`Style::for_stdout`] strips the
/// color when stdout is not a terminal.
pub fn suggest_command_tip<P: Progress>(user_data: &UserData, progress: &mut P) {
    let (command, description) = tip_for_session(user_data.session_count());
    let line = format!(">> Try `{command}`: {description}");
    let styled = Style::new().color256(TIP_COLOR).for_stdout().apply_to(line);
    progress.add_to_print_buffer(&format!("\n{styled}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_through_every_tip() {
        let count = COMMAND_TIPS.len() as u32;
        let mut seen = (0..count)
            .map(|session| tip_for_session(session).0)
            .collect::<Vec<_>>();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), COMMAND_TIPS.len());
        assert_eq!(tip_for_session(0), tip_for_session(count));
    }
}
