use std::{
    collections::HashSet,
    fmt::{self, Display},
    sync::LazyLock,
};

use mirrord_config::{util::VecOrSingle, LayerConfig};
use tracing::trace;

use crate::error::LayerError;

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
        "jspawnhelper",
    ])
});

/// Credentials of the process the layer is injected into.
pub struct ExecutableName {
    /// Executable file name, for example `x86_64-linux-gnu-ld.bfd`.
    exec_name: String,
    /// Last part of the process name as seen in the arguments, for example `ld` extracted from
    /// `/usr/bin/ld`.
    invoked_as: String,
}

impl ExecutableName {
    /// Creates a new instance of this struct using [`std::env::current_exe`] and
    /// [`std::env::args`].
    pub(crate) fn from_env() -> Result<Self, LayerError> {
        let exec_name = std::env::current_exe()
            .ok()
            .and_then(|arg| {
                arg.file_name()
                    .and_then(|os_str| os_str.to_str())
                    .map(String::from)
            })
            .ok_or(LayerError::NoProcessFound)?;

        let invoked_as = std::env::args()
            .next()
            .as_ref()
            .and_then(|arg| arg.rsplit('/').next())
            .ok_or(LayerError::NoProcessFound)?
            .to_string();

        Ok(Self {
            exec_name,
            invoked_as,
        })
    }

    #[cfg(target_os = "macos")]
    pub fn ends_with(&self, suffix: &str) -> bool {
        self.exec_name.ends_with(suffix) || self.invoked_as.ends_with(suffix)
    }

    fn is_build_tool(&self) -> bool {
        BUILD_TOOL_PROCESSES.contains(self.exec_name.as_str())
            || BUILD_TOOL_PROCESSES.contains(self.invoked_as.as_str())
    }

    /// Checks if mirrord-layer should load with this process.
    ///
    /// ## Details
    ///
    /// Some processes may start other processes (like an IDE launching a program to be debugged),
    /// and we don't want to hook mirrord-layer into those.
    fn should_load<S: AsRef<str>>(&self, skip_processes: &[S], skip_build_tools: bool) -> bool {
        if skip_build_tools && self.is_build_tool() {
            return false;
        }

        !skip_processes
            .iter()
            .any(|name| name.as_ref() == self.exec_name || name.as_ref() == self.invoked_as)
    }

    /// Determine the [`LoadType`] for this process.
    pub fn load_type(&self, config: LayerConfig) -> LoadType {
        let skip_processes = config
            .skip_processes
            .as_ref()
            .map(VecOrSingle::as_slice)
            .unwrap_or(&[]);

        if self.should_load(skip_processes, config.skip_build_tools) {
            trace!("Loading into process: {self}.");
            LoadType::Full(Box::new(config))
        } else {
            #[cfg(target_os = "macos")]
            if sip::is_sip_only(self) {
                trace!("Loading into process: {self}, but only hooking exec/spawn.");
                return LoadType::SIPOnly(Box::new(config));
            }

            trace!("Not loading into process: {self}.");
            LoadType::Skip
        }
    }
}

impl Display for ExecutableName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} invoked as {}", self.exec_name, self.invoked_as)
    }
}

/// For processes that spawn other processes and also specified in `MIRRORD_SKIP_PROCESSES` list we
/// should patch sip for the spawned instances, curretly limiting to list from
/// `SIP_ONLY_PROCESSES`
#[cfg(target_os = "macos")]
mod sip {
    use super::*;

    static SIP_ONLY_PROCESSES: LazyLock<HashSet<&str>> =
        LazyLock::new(|| HashSet::from(["sh", "bash", "env", "go", "dlv"]));

    pub fn is_sip_only(given_process: &ExecutableName) -> bool {
        given_process.is_build_tool()
            || SIP_ONLY_PROCESSES.contains(given_process.exec_name.as_str())
            || SIP_ONLY_PROCESSES.contains(given_process.invoked_as.as_str())
    }
}

/// Load Type of mirrord-layer
pub enum LoadType {
    /// Mirrord is loaded fully and layer should connect to agent
    Full(Box<LayerConfig>),
    /// Only load sip patch required hooks
    #[cfg(target_os = "macos")]
    SIPOnly(Box<LayerConfig>),

    /// Skip on current process
    Skip,
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("test", "test", &["foo"], false)]
    #[case("test", "test", &[], false)]
    #[case("test", "test", &["foo", "bar", "baz"], false)]
    #[case("cargo", "cargo", &[], false)]
    #[case("cargo", "cargo", &["foo"], false)]
    #[case("x86_64-linux-gnu-ld.bfd", "ld", &[], false)]
    fn should_load_true(
        #[case] exec_name: &str,
        #[case] invoked_as: &str,
        #[case] skip_processes: &[&str],
        #[case] skip_build_tools: bool,
    ) {
        let executable_name = ExecutableName {
            exec_name: exec_name.to_string(),
            invoked_as: invoked_as.to_string(),
        };

        assert!(executable_name.should_load(skip_processes, skip_build_tools));
    }

    #[rstest]
    #[case("test", "test", &["test"], false)]
    #[case("test", "test", &["test", "foo", "bar", "baz"], false)]
    #[case("cargo", "cargo", &[], true)]
    #[case("cargo", "cargo", &["foo"], true)]
    #[case("x86_64-linux-gnu-ld.bfd", "ld", &["ld"], false)]
    #[case("x86_64-linux-gnu-ld.bfd", "ld", &[], true)]
    fn should_load_false(
        #[case] exec_name: &str,
        #[case] invoked_as: &str,
        #[case] skip_processes: &[&str],
        #[case] skip_build_tools: bool,
    ) {
        let executable_name = ExecutableName {
            exec_name: exec_name.to_string(),
            invoked_as: invoked_as.to_string(),
        };

        assert!(!executable_name.should_load(skip_processes, skip_build_tools));
    }
}
