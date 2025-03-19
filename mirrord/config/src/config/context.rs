use std::{
    collections::HashMap,
    env::VarError,
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
};

/// Context for generating and verifying a [`MirrordConfig`](super::MirrordConfig).
///
/// See:
/// 1. [`LayerConfig::verify`](crate::LayerConfig::verify)
/// 2. [`MirrordConfig::generate_config`](crate::config::MirrordConfig::generate_config)
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
}

impl ConfigContext {
    /// Adds an override for an environment variable.
    ///
    /// This override will only affect [`Self::get_env`] behavior,
    /// it will **not** change the process environment.
    pub fn override_env<K: AsRef<OsStr>, V: AsRef<OsStr>>(mut self, key: K, value: V) -> Self {
        self.env_override
            .insert(key.as_ref().into(), value.as_ref().into());
        self
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

    /// Returns value of an environment variable with the given name.
    ///
    /// This is the only way we should read environment when generating or verifying configuration.
    pub fn get_env(&self, name: &str) -> Result<String, VarError> {
        let name = OsStr::from_bytes(name.as_bytes());

        let os_value = match self.env_override.get(name) {
            Some(value) => Ok(value.clone()),
            None if self.strict_env => Err(VarError::NotPresent),
            None => std::env::var_os(name).ok_or(VarError::NotPresent),
        }?;

        std::str::from_utf8(os_value.as_bytes())
            .map(ToString::to_string)
            .map_err(|_| VarError::NotUnicode(os_value))
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
}

impl Default for ConfigContext {
    fn default() -> Self {
        Self {
            empty_target_final: true,
            env_override: Default::default(),
            strict_env: false,
            warnings: Default::default(),
        }
    }
}
