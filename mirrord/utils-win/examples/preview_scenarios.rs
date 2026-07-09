//! Preview each crash-dialog scenario without a real crash.
//!
//! Drives the real `report::surface` path (same title/subtitle/body and artifacts the monitor
//! produces), so each scenario shows exactly the dialog a user would see.
//!
//! Run: `cargo run -p utils-win --example preview_scenarios -- <scenario>`
//! where `<scenario>` is one of: `crash`, `kill`, `fastfail`, `initfail`.

#[cfg(windows)]
fn main() {
    use utils_win::diagnostics::report::{CrashReport, Outcome, ProcessNode, surface};

    let scenario = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "crash".to_owned());

    // The registered layer'd processes: an intermediate node.exe and the focused crasher child. The
    // CLI root (pid 33828) is not a layer'd process, so it never registers — it is only the tree's
    // header line, not a node here.
    let nodes = vec![
        ProcessNode {
            pid: 19580,
            parent_pid: 33828,
            name: "node.exe".to_owned(),
            role: "parent".to_owned(),
            log_path: None,
            // Already dead — shows up in the tree marked, e.g. the init-failure dead-parent case.
            exit_code: Some(0xC000_0409),
        },
        ProcessNode {
            pid: 32908,
            parent_pid: 19580,
            name: "crasher.exe".to_owned(),
            role: "child".to_owned(),
            log_path: None,
            exit_code: None,
        },
    ];

    let outcome = match scenario.as_str() {
        // A handled native fault: access violation. Title -> "mirrord caught a crash".
        "crash" => Outcome::Crashed,
        // An external kill (TerminateProcess, exit 0). Title -> "A process was terminated".
        "kill" => Outcome::Terminated { exit_code: 0 },
        // A fail-fast (0xC0000409) the handler missed. Title -> "mirrord caught a crash".
        "fastfail" => Outcome::Terminated {
            exit_code: 0xC000_0409,
        },
        // An early layer-init failure. Title -> "mirrord layer failed to start".
        "initfail" => Outcome::InitFailed {
            reason: "No init event found for pid 32908 (role=child, parent pid=19580 (node.exe) \
                     dead exit=0xc0000409)"
                .to_owned(),
        },
        other => {
            eprintln!("unknown scenario '{other}' (use: crash | kill | fastfail | initfail)");
            return;
        }
    };

    let directory = std::env::temp_dir().join("mirrord-preview");
    let _ = std::fs::create_dir_all(&directory);
    let dump_path = directory.join("mirrord-crash_preview_crasher_pid32908.dmp");
    let dump = matches!(scenario.as_str(), "crash").then_some(dump_path.as_path());

    let report = CrashReport {
        focus_pid: 32908,
        focus_name: "crasher.exe",
        outcome,
        nodes: &nodes,
        root_pid: 33828,
        dump,
        record: None,
        logs: &[],
        modules: None,
        mirrord_version: "3.216.0",
        stem: "mirrord-crash_preview_crasher_pid32908",
    };

    // `true` claims the dialog slot; `surface` still only shows it on an interactive desktop.
    surface(&report, &directory, true);
}

#[cfg(not(windows))]
fn main() {}
