use std::{collections::HashSet, sync::LazyLock};

use mirrord_config::{util::VecOrSingle, LayerConfig};
use tracing::trace;

static BUILD_TOOL_PROCESSES: LazyLock<HashSet<&str>> = LazyLock::new(|| {
    HashSet::from([
        "as",
        "cc",
        "ld",
        "go",
        "air",
        "asm",
        "cc1",
        "cgo",
        "dlv",
        "gcc",
        "git",
        "link",
        "math",
        "cargo",
        "hpack",
        "rustc",
        "compile",
        "collect2",
        "cargo-watch",
        "debugserver",
    ])
});

fn is_build_tool(given_process: &str) -> bool {
    BUILD_TOOL_PROCESSES.contains(given_process)
}

/// For processes that spawn other processes and also specified in `MIRRORD_SKIP_PROCESSES` list we
/// should patch sip for the spawned instances, curretly limiting to list from
/// `SIP_ONLY_PROCESSES`
#[cfg(target_os = "macos")]
mod sip {
    use super::*;

    static SIP_ONLY_PROCESSES: LazyLock<HashSet<&str>> =
        LazyLock::new(|| HashSet::from(["sh", "bash", "env"]));

    pub fn is_sip_only(given_process: &str) -> bool {
        is_build_tool(given_process) || SIP_ONLY_PROCESSES.contains(given_process)
    }
}

/// Load Type of mirrord-layer
pub enum LoadType {
    /// Mirrord is loaded fully and layer should connect to agent
    Full(Box<LayerConfig>),
    /// Only load sip patch required hooks
    #[cfg(target_os = "macos")]
    SIPOnly,

    /// Skip on current process
    Skip,
}

/// Determine the load type for the `given_process` with the help of [`should_load`]
pub fn load_type(given_process: &str, config: LayerConfig) -> LoadType {
    let skip_processes = config.skip_processes.clone().map(VecOrSingle::to_vec);

    if should_load(given_process, skip_processes, config.skip_build_tools) {
        trace!("Loading into process: {given_process}.");
        LoadType::Full(Box::new(config))
    } else {
        #[cfg(target_os = "macos")]
        if sip::is_sip_only(given_process) {
            trace!("Loading into process: {given_process}, but only hooking exec/spawn.");
            return LoadType::SIPOnly;
        }

        trace!("Not loading into process: {given_process}.");
        LoadType::Skip
    }
}

/// Checks if mirrord-layer should load with the process named `given_process`.
///
/// ## Details
///
/// Some processes may start other processes (like an IDE launching a program to be debugged), and
/// we don't want to hook mirrord-layer into those.
fn should_load(
    given_process: &str,
    skip_processes: Option<Vec<String>>,
    skip_build_tools: bool,
) -> bool {
    if skip_build_tools && is_build_tool(given_process) {
        return false;
    }

    if let Some(processes_to_avoid) = skip_processes {
        !processes_to_avoid.iter().any(|x| x == given_process)
    } else {
        true
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("test", Some(vec!["foo".to_string()]), false)]
    #[case("test", None, false)]
    #[case("test", Some(vec!["foo".to_owned(), "bar".to_owned(), "baz".to_owned()]), false)]
    #[case("cargo", None, false)]
    #[case("cargo", Some(vec!["foo".to_string()]), false)]
    fn should_load_true(
        #[case] given_process: &str,
        #[case] skip_processes: Option<Vec<String>>,
        #[case] skip_build_tools: bool,
    ) {
        assert!(should_load(given_process, skip_processes, skip_build_tools));
    }

    #[rstest]
    #[case("test", Some(vec!["test".to_string()]), false)]
    #[case("test", Some(vec!["test".to_owned(), "foo".to_owned(), "bar".to_owned(), "baz".to_owned()]), false)]
    #[case("cargo", None, true)]
    #[case("cargo", Some(vec!["foo".to_string()]), true)]
    fn should_load_false(
        #[case] given_process: &str,
        #[case] skip_processes: Option<Vec<String>>,
        #[case] skip_build_tools: bool,
    ) {
        assert!(!should_load(
            given_process,
            skip_processes,
            skip_build_tools
        ));
    }
}
