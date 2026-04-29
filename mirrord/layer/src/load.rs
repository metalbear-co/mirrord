use std::{
    collections::HashSet,
    ffi::OsString,
    fmt::{self, Display},
    sync::LazyLock,
};

use mirrord_config::LayerConfig;
use mirrord_intproxy_protocol::ProcessInfo;
use mirrord_layer_lib::error::LayerError;
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
        "jspawnhelper",
        "bazel-real",
        "clang",
    ])
});

/// Credentials of the process the layer is injected into.
#[derive(Debug)]
pub struct ExecuteArgs {
    /// Executable file name, for example `x86_64-linux-gnu-ld.bfd`.
    exec_name: String,
    /// Last part of the process name as seen in the arguments, for example `ld` extracted from
    /// `/usr/bin/ld`.
    invoked_as: String,
    /// Executable arguments
    pub(crate) args: Vec<OsString>,
}

impl ExecuteArgs {
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

        let args = std::env::args_os().collect();
        let invoked_as = std::env::args()
            .next()
            .as_ref()
            .and_then(|arg| arg.rsplit('/').next())
            .ok_or(LayerError::NoProcessFound)?
            .to_string();

        Ok(Self {
            exec_name,
            invoked_as,
            args,
        })
    }

    #[cfg(target_os = "macos")]
    pub fn ends_with(&self, suffix: &str) -> bool {
        self.exec_name.ends_with(suffix) || self.invoked_as.ends_with(suffix)
    }

    fn is_build_tool(&self, skip_extra_build_tools: Option<&[String]>) -> bool {
        let mut skip_build_tools = HashSet::<_>::from_iter(
            skip_extra_build_tools
                .map(<[_]>::to_vec)
                .unwrap_or_default(),
        );

        skip_build_tools.extend(BUILD_TOOL_PROCESSES.iter().map(ToString::to_string));

        skip_build_tools.contains(self.exec_name.as_str())
            || skip_build_tools.contains(self.invoked_as.as_str())
    }

    /// Checks if mirrord-layer should load with this process.
    ///
    /// ## Details
    ///
    /// Some processes may start other processes (like an IDE launching a program to be debugged),
    /// and we don't want to hook mirrord-layer into those.
    fn should_load<S: AsRef<str>>(
        &self,
        skip_processes: &[S],
        skip_build_tools: bool,
        skip_extra_build_tools: Option<&[String]>,
    ) -> bool {
        if skip_build_tools && self.is_build_tool(skip_extra_build_tools) {
            return false;
        }

        // ignore intellij debugger https://github.com/metalbear-co/mirrord/issues/2408
        // don't put it in build tools since we don't want to SIP load on macOS. (leads to above
        // issue)
        if self
            .exec_name
            .as_str()
            .ends_with("JetBrains.Debugger.Worker")
            || self
                .invoked_as
                .as_str()
                .ends_with("JetBrains.Debugger.Worker")
        {
            return false;
        }

        !skip_processes
            .iter()
            .any(|name| name.as_ref() == self.exec_name || name.as_ref() == self.invoked_as)
    }

    /// Determine the [`LoadType`] for this process.
    pub fn load_type(&self, config: &LayerConfig) -> LoadType {
        let skip_processes = config.skip_processes.as_deref().unwrap_or(&[]);

        if self.should_load(
            skip_processes,
            config.skip_build_tools,
            config.skip_extra_build_tools.as_deref(),
        ) {
            trace!("Loading into process: {self}.");
            LoadType::Full
        } else {
            #[cfg(target_os = "macos")]
            if sip::is_sip_only(self, config.skip_extra_build_tools.as_deref()) {
                trace!("Loading into process: {self}, but only hooking exec/spawn.");
                return LoadType::SIPOnly;
            }

            trace!("Not loading into process: {self}.");
            LoadType::Skip
        }
    }

    pub(crate) fn to_process_info(&self, config: &LayerConfig) -> ProcessInfo {
        ProcessInfo {
            pid: nix::unistd::getpid().as_raw(),
            parent_pid: nix::unistd::getppid().as_raw(),
            name: self.exec_name.clone(),
            cmdline: self
                .args
                .iter()
                .map(|arg| arg.to_string_lossy().to_string())
                .collect(),
            loaded: matches!(self.load_type(config), LoadType::Full),
        }
    }
}

impl Display for ExecuteArgs {
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

    pub fn is_sip_only(
        given_process: &ExecuteArgs,
        skip_extra_build_tools: Option<&[String]>,
    ) -> bool {
        given_process.is_build_tool(skip_extra_build_tools)
            || SIP_ONLY_PROCESSES.contains(given_process.exec_name.as_str())
            || SIP_ONLY_PROCESSES.contains(given_process.invoked_as.as_str())
    }
}

/// Load Type of mirrord-layer
pub enum LoadType {
    /// Mirrord is loaded fully and layer should connect to agent
    Full,

    /// Only load sip patch required hooks
    #[cfg(target_os = "macos")]
    SIPOnly,

    /// Skip on current process, make only a dummy connection to the internal proxy (to prevent
    /// timeouts)
    Skip,
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("test", "test", &["foo"], false, Some(vec!["KonradIofMasovia".to_string()]))]
    #[case("test", "test", &[], false, None)]
    #[case("test", "test", &["foo", "bar", "baz"], false, Some(vec!["HenryIIthePious".to_string()]))]
    #[case("cargo", "cargo", &[], false, Some(vec!["BolesławIItheHorned".to_string()]))]
    #[case("cargo", "cargo", &["foo"], false, Some(vec!["HenrykIVProbus".to_string()]))]
    #[case("x86_64-linux-gnu-ld.bfd", "ld", &[], false, Some(vec!["PrzemysłII".to_string()]))]
    #[case("test", "test", &[], true, Some(vec!["HenryItheBearded".to_string()]))]
    fn should_load_true(
        #[case] exec_name: &str,
        #[case] invoked_as: &str,
        #[case] skip_processes: &[&str],
        #[case] skip_build_tools: bool,
        #[case] skip_extra_build_tools: Option<Vec<String>>,
    ) {
        let executable_name = ExecuteArgs {
            exec_name: exec_name.to_string(),
            invoked_as: invoked_as.to_string(),
            args: Vec::new(),
        };

        assert!(executable_name.should_load(
            skip_processes,
            skip_build_tools,
            skip_extra_build_tools.as_deref()
        ));
    }

    #[rstest]
    #[case("test", "test", &["test"], false, None)]
    #[case("test", "test", &["test", "foo", "bar", "baz"], false, Some(vec!["KonradIofMasovia".to_string()]))]
    #[case("cargo", "cargo", &[], true, None)]
    #[case("cargo", "cargo", &["foo"], true, Some(vec!["HenryItheBearded".to_string()]))]
    #[case("x86_64-linux-gnu-ld.bfd", "ld", &["ld"], false, Some(vec!["BolesławIItheHorned".to_string()]))]
    #[case("x86_64-linux-gnu-ld.bfd", "ld", &[], true, None)]
    #[case("PrzemysłII", "PrzemysłII", &[], true, Some(vec!["PrzemysłII".to_string()]))]
    fn should_load_false(
        #[case] exec_name: &str,
        #[case] invoked_as: &str,
        #[case] skip_processes: &[&str],
        #[case] skip_build_tools: bool,
        #[case] skip_extra_build_tools: Option<Vec<String>>,
    ) {
        let executable_name = ExecuteArgs {
            exec_name: exec_name.to_string(),
            invoked_as: invoked_as.to_string(),
            args: Vec::new(),
        };

        assert!(!executable_name.should_load(
            skip_processes,
            skip_build_tools,
            skip_extra_build_tools.as_deref()
        ));
    }
}
