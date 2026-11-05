#[cfg(not(target_os = "windows"))]
use std::os::unix::ffi::OsStrExt;
use std::{
    collections::HashMap,
    env::VarError,
    ffi::{OsStr, OsString},
    ops::Not,
    path::{Path, PathBuf},
};

/// Context for generating and verifying a [`MirrordConfig`](super::MirrordConfig).
///
/// See:
/// 1. [`LayerConfig::verify`](crate::LayerConfig::verify)
/// 2. [`MirrordConfig::generate_config`](crate::config::MirrordConfig::generate_config)
#[derive(Default)]
pub struct ConfigContext {
    /// Whether an empty [TargetConfig::path](crate::target::TargetConfig::path) should be
    /// considered final.
    ///
    /// This might not always be the case, for example the IDE extensions
    /// verify the config with `mirrord verify-config`,
    /// before they allow the user to select the target from a quick pick or a dialog.
    empty_target_final: bool,

    /// Environment variables that override the process environment ([`mod@std::env`]).
    env_override: HashMap<OsString, OsString>,

    /// If true, use only [`Self::env_override`] strictly without [`mod@std::env`].
    strict_env: bool,

    /// Warnings collected during config verification.
    warnings: Vec<String>,

    /// The directory mirrord is started from (`mirrord exec` or `mirrord up`).
    /// `payload_protobuf` resolves its relative paths from it with [`Self::resolve_path`]. When
    /// [`None`], they resolve from the working directory of this process.
    ///
    /// Set when the config is checked by a process that doesn't run in that directory, such as
    /// `mirrord mcp`, whose working directory is chosen by the MCP client.
    invocation_directory: Option<PathBuf>,
}

impl ConfigContext {
    /// Adds an override for an environment variable.
    ///
    /// This override will only affect [`Self::get_env`] behavior,
    /// it will **not** change the process environment.
    pub fn override_env<K: AsRef<OsStr>, V: AsRef<OsStr>>(mut self, key: K, value: V) -> Self {
        self.override_env_mut(key, value);
        self
    }

    /// Adds an override for an environment variable via mutable reference.
    ///
    /// Like [`override_env`](Self::override_env), but takes `&mut self` instead of consuming
    /// `self`. Useful when you only have a mutable reference to the context.
    ///
    /// This override will only affect [`Self::get_env`] behavior,
    /// it will **not** change the process environment.
    pub fn override_env_mut<K: AsRef<OsStr>, V: AsRef<OsStr>>(&mut self, key: K, value: V) {
        self.env_override
            .insert(key.as_ref().into(), value.as_ref().into());
    }

    /// Adds overrides for multiple environment variables.
    ///
    /// These overrides will only affect [`Self::get_env`] behavior,
    /// they will **not** change the process environment.
    pub fn override_envs<K: AsRef<OsStr>, V: AsRef<OsStr>, I: IntoIterator<Item = (K, V)>>(
        mut self,
        envs: I,
    ) -> Self {
        for (key, value) in envs {
            self.env_override
                .insert(key.as_ref().into(), value.as_ref().into());
        }
        self
    }

    /// If the given `value` is [`Some`], adds an override for an environment variable.
    ///
    /// Otherwise, does nothing.
    ///
    /// This override will only affect [`Self::get_env`] behavior,
    /// it will **not** change the process environment.
    pub fn override_env_opt<K: AsRef<OsStr>, V: AsRef<OsStr>>(
        mut self,
        key: K,
        value: Option<V>,
    ) -> Self {
        if let Some(value) = value {
            self.env_override
                .insert(key.as_ref().into(), value.as_ref().into());
        }

        self
    }

    /// Disables usage of [`mod@std::env`] in [`Self::get_env`].
    ///
    /// Effectively isolates config generation/verification from process environment.
    pub fn strict_env(mut self, value: bool) -> Self {
        self.strict_env = value;
        self
    }

    /// Marks whether an empty [TargetConfig::path](crate::target::TargetConfig::path) should be
    /// considered final when verifying config.
    pub fn empty_target_final(mut self, value: bool) -> Self {
        self.empty_target_final = value;
        self
    }

    /// Sets the directory mirrord is started from, which [`Self::resolve_path`] resolves relative
    /// paths from. [`None`] keeps the working directory of this process.
    pub fn invocation_directory(mut self, directory: Option<PathBuf>) -> Self {
        self.invocation_directory = directory;
        self
    }

    /// The absolute path of a path given in the config, which is relative to the directory set
    /// with [`Self::invocation_directory`] or else to the working directory of this process.
    ///
    /// The result is absolute in both cases, so code that compares paths with each other, such as
    /// protox matching a `.proto` file with its include directories, sees the same paths whether
    /// or not a directory is set.
    pub fn resolve_path(&self, path: &Path) -> PathBuf {
        // Joined to `.` rather than left as it is, so an empty path is the directory itself in
        // both cases, where `std::path::absolute` would reject it.
        let path = self
            .invocation_directory
            .as_deref()
            .unwrap_or(Path::new("."))
            .join(path);
        std::path::absolute(&path).unwrap_or(path)
    }

    /// Returns value of an environment variable with the given name.
    ///
    /// This is the only way we should read environment when generating or verifying configuration.
    pub fn get_env(&self, name: &str) -> Result<String, VarError> {
        #[cfg(not(target_os = "windows"))]
        let name = OsStr::from_bytes(name.as_bytes());

        #[cfg(target_os = "windows")]
        let name = OsStr::new(name);

        let os_value = match self.env_override.get(name) {
            Some(value) => Ok(value.clone()),
            None if self.strict_env => Err(VarError::NotPresent),
            None => std::env::var_os(name).ok_or(VarError::NotPresent),
        }?;

        #[cfg(not(target_os = "windows"))]
        let s = std::str::from_utf8(os_value.as_bytes()).map(ToOwned::to_owned);
        #[cfg(target_os = "windows")]
        let s = os_value.clone().into_string();

        s.map_err(|_| VarError::NotUnicode(os_value))
    }

    /// Returns the mark previously set with [`ConfigContext::empty_target_final`].
    pub fn is_empty_target_final(&self) -> bool {
        self.empty_target_final
    }

    /// Stores a warning produced when verifying a config.
    pub fn add_warning(&mut self, warning: String) {
        self.warnings.push(warning);
    }

    /// Returns all warnings previously stored with [`ConfigContext::add_warning`].
    pub fn into_warnings(self) -> Vec<String> {
        self.warnings
    }

    /// Returns whether this context stores any warnings.
    pub fn has_warnings(&self) -> bool {
        self.warnings.is_empty().not()
    }
}
