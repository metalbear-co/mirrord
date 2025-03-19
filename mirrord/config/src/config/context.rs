use std::{
    collections::HashMap,
    env::VarError,
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt,
};

/// Context for generating and verifying a [`MirrordConfig`](super::MirrordConfig).
pub struct ConfigContext {
    /// Whether empty target configuration is final.
    ///
    /// This might not always be the case.
    /// Right now the IDE extensions verify the config with `mirrord verify-config`,
    /// before they allow the user to select the target.
    empty_target_final: bool,
    /// Environment variables that override the process environment.
    env_override: HashMap<OsString, Option<OsString>>,
    /// If true, use only [`Self::env_override`] strictly without [`mod@std::env`]
    strict_env: bool,
    /// Warnings collected during [`LayerConfig`](crate::LayerConfig) verification.
    warnings: Vec<String>,
}

impl ConfigContext {
    pub fn override_env<K: AsRef<OsStr>, V: AsRef<OsStr>>(
        mut self,
        key: K,
        value: Option<V>,
    ) -> Self {
        let value = value.map(|v| v.as_ref().into());
        self.env_override.insert(key.as_ref().into(), value);
        self
    }

    pub fn strict_env(mut self, value: bool) -> Self {
        self.strict_env = value;
        self
    }

    pub fn empty_target_final(mut self, value: bool) -> Self {
        self.empty_target_final = value;
        self
    }

    pub fn get_env(&self, name: &str) -> Result<String, VarError> {
        let name = OsStr::from_bytes(name.as_bytes());

        let os_value = match self.env_override.get(name) {
            Some(Some(value)) => Ok(value.clone()),
            Some(None) => Err(VarError::NotPresent),
            None if self.strict_env => Err(VarError::NotPresent),
            None => std::env::var_os(name).ok_or(VarError::NotPresent),
        }?;

        std::str::from_utf8(os_value.as_bytes())
            .map(ToString::to_string)
            .map_err(|_| VarError::NotUnicode(os_value))
    }

    pub fn is_empty_target_final(&self) -> bool {
        self.empty_target_final
    }

    pub fn add_warning(&mut self, warning: String) {
        self.warnings.push(warning);
    }

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
