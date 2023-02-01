use mirrord_config::{util::VecOrSingle, LayerConfig};

#[cfg(target_os = "macos")]
mod sip {
    use std::{collections::HashSet, sync::LazyLock};

    static SIP_EXEC_PROCESSES: LazyLock<HashSet<&str>> =
        LazyLock::new(|| HashSet::from(["sh", "bash", "cargo", "rustc", "cargo-watch"]));

    pub fn is_sip_exec(given_process: &str) -> bool {
        SIP_EXEC_PROCESSES.contains(given_process)
    }
}

pub enum LoadType {
    Full(Box<LayerConfig>),
    #[cfg(target_os = "macos")]
    SIPExecve,
    Skipped,
}

pub fn load_type(given_process: &str, config: LayerConfig) -> LoadType {
    let skip_processes = config.skip_processes.clone().map(VecOrSingle::to_vec);

    if should_load(given_process, skip_processes) {
        LoadType::Full(Box::new(config))
    } else {
        #[cfg(target_os = "macos")]
        if sip::is_sip_exec(given_process) {
            return LoadType::SIPExecve;
        }

        LoadType::Skipped
    }
}

/// Checks if mirrord-layer should load with the process named `given_process`.
///
/// ## Details
///
/// Some processes may start other processes (like an IDE launching a program to be debugged), and
/// we don't want to hook mirrord-layer into those.
fn should_load(given_process: &str, skip_processes: Option<Vec<String>>) -> bool {
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
    #[case("test", Some(vec!["foo".to_string()]))]
    #[case("test", None)]
    #[case("test", Some(vec!["foo".to_owned(), "bar".to_owned(), "baz".to_owned()]))]
    fn test_should_load_true(
        #[case] given_process: &str,
        #[case] skip_processes: Option<Vec<String>>,
    ) {
        assert!(should_load(given_process, skip_processes));
    }

    #[rstest]
    #[case("test", Some(vec!["test".to_string()]))]
    #[case("test", Some(vec!["test".to_owned(), "foo".to_owned(), "bar".to_owned(), "baz".to_owned()]))]
    fn test_should_load_false(
        #[case] given_process: &str,
        #[case] skip_processes: Option<Vec<String>>,
    ) {
        assert!(!should_load(given_process, skip_processes));
    }
}
