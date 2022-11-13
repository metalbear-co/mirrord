use std::collections::{HashMap, HashSet};

use kube::Config;

const MIRRORD_GUARDED_ENVS: &str = "MIRRORD_GUARDED_ENVS";

#[derive(Debug)]
pub struct EnvVarGuard {
    envs: HashMap<String, String>,
}

impl EnvVarGuard {
    #[cfg(target_os = "linux")]
    const ENV_VAR: &str = "LD_PRELOAD";
    #[cfg(target_os = "macos")]
    const ENV_VAR: &str = "DYLD_INSERT_LIBRARIES";

    pub fn new() -> Self {
        let envs = std::env::var(MIRRORD_GUARDED_ENVS)
            .ok()
            .and_then(|orig| serde_json::from_str(&orig).ok())
            .unwrap_or_else(|| {
                let fork_args = std::env::vars()
                    .filter(|(key, _)| key != MIRRORD_GUARDED_ENVS && key != EnvVarGuard::ENV_VAR)
                    .collect();

                if let Ok(ser_args) = serde_json::to_string(&fork_args) {
                    std::env::set_var(MIRRORD_GUARDED_ENVS, ser_args);
                }

                fork_args
            });

        Self { envs }
    }

    pub fn prepare_config(&self, config: &mut Config) {
        if let Some(mut exec) = config.auth_info.exec.as_mut() {
            match &mut exec.env {
                Some(envs) => self.extend_kube_env(envs),
                None => exec.env = Some(self.kube_env()),
            }

            exec.drop_env = Some(self.dropped_env());
        }
        if let Some(auth_provider) = config.auth_info.auth_provider.as_mut() {
            if auth_provider.config.contains_key("cmd-path") {
                auth_provider
                    .config
                    .insert("cmd-drop-env".to_string(), self.dropped_env().join(" "));
            }
        }
    }

    fn kube_env(&self) -> Vec<HashMap<String, String>> {
        let mut envs = Vec::new();
        self.extend_kube_env(&mut envs);
        envs
    }

    fn extend_kube_env(&self, envs: &mut Vec<HashMap<String, String>>) {
        let filtered: HashSet<_> = envs
            .iter()
            .filter_map(|item| item.get("name"))
            .cloned()
            .collect();

        for (key, value) in self
            .envs
            .iter()
            .filter(|(key, _)| !filtered.contains(key.as_str()))
        {
            let mut env = HashMap::new();
            env.insert("name".to_owned(), key.clone());
            env.insert("value".to_owned(), value.clone());

            envs.push(env);
        }
    }

    fn dropped_env(&self) -> Vec<String> {
        std::env::vars()
            .map(|(key, _)| key)
            .filter(|key| !self.envs.contains_key(key.as_str()))
            .collect()
    }

    #[cfg(test)]
    pub fn is_correct_auth_env(
        &self,
        envs: &HashMap<String, String>,
    ) -> Result<(), std::io::Error> {
        for (key, value) in envs {
            let orig_val = self.envs.get(key).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Missing env value {}", key),
                )
            })?;

            if value != orig_val {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!(
                        "Missmatch in values recived {} expected {}",
                        value, orig_val
                    ),
                ));
            }
        }

        Ok(())
    }
}
