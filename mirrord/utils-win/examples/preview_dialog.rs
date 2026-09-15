//! Preview the crash dialog without a real crash.
//!
//! Run: `cargo run -p utils-win --example preview_dialog`

#[cfg(windows)]
fn main() {
    use std::path::Path;

    let title = "mirrord caught a crash";
    let subtitle = "crasher (pid 32908) · access violation (0xc0000005)";

    let body = "\
mirrord has hit an unexpected problem!

\"[But it worked in the cluster!](https://minecraft.wiki/w/Crash)\"

>> Reach out to us directly on [Slack](https://metalbear.com/slack) (ask in #mirrord-help)
>> Or file a [bug report on GitHub](https://github.com/metalbear-co/mirrord/issues/new/choose)
>> Or email us at [hi@metalbear.com](mailto:hi@metalbear.com)

A process running under mirrord crashed: crasher(pid 32908).
mirrord version: 3.216.0

Process tree:
mirrord(33828)
  crasher(32908) [parent] X crashed
";

    utils_win::diagnostics::dialog::show(
        title,
        subtitle,
        body,
        Path::new("C:\\mirrord_log"),
        "https://github.com/metalbear-co/mirrord/issues/new/choose",
    );
}

#[cfg(not(windows))]
fn main() {}
