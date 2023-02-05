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
            // This env is added by kube-rs to executed process, so it's okay to ignore it
            if key == "KUBERNETES_EXEC_INFO" {
                continue;
            }

            let orig_val = self.envs.get(key).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Missing env value {key}"),
                )
            })?;

            if value != orig_val {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    format!("Missmatch in values recived {value} expected {orig_val}"),
                ));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use http_body::Empty;
    use hyper::body::Body;
    use k8s_openapi::http::{header::AUTHORIZATION, Request, Response};
    use kube::{
        client::ConfigExt,
        config::{AuthInfo, ExecConfig},
        Client,
    };
    use tower::{service_fn, ServiceBuilder};

    use super::*;

    #[tokio::test]
    async fn correct_envs_kubectl() {
        std::env::set_var("MIRRORD_TEST_ENV_VAR_KUBECTL", "true");

        let _guard = EnvVarGuard::new();
        let mut config = Config {
            accept_invalid_certs: true,
            auth_info: AuthInfo {
                exec: Some(ExecConfig {
                    api_version: Some("client.authentication.k8s.io/v1beta1".to_owned()),
                    command: Some("node".to_owned()),
                    args: Some(vec!["../layer/tests/apps/kubectl/auth-util.js".to_owned()]),
                    env: None,
                    drop_env: None,
                    interactive_mode: None,
                }),
                ..Default::default()
            },
            ..Config::new("https://kubernetes.docker.internal:6443".parse().unwrap())
        };

        _guard.prepare_config(&mut config);

        let _guard = Arc::new(_guard);

        let service = ServiceBuilder::new()
            .layer(config.base_uri_layer())
            .option_layer(config.auth_layer().unwrap())
            .service(service_fn(move |req: Request<Body>| {
                let _guard = _guard.clone();
                async move {
                    let auth_env_vars = req
                        .headers()
                        .get(AUTHORIZATION)
                        .and_then(|value| value.to_str().ok())
                        .and_then(|token| token.strip_prefix("Bearer "))
                        .and_then(|token| base64::decode(token).ok())
                        .and_then(|value| {
                            serde_json::from_slice::<HashMap<String, String>>(&value).ok()
                        })
                        .ok_or_else(|| {
                            std::io::Error::new(std::io::ErrorKind::Other, "No Auth Header Sent")
                        })?;

                    _guard
                        .is_correct_auth_env(&auth_env_vars)
                        .map(|_| Response::new(Empty::new()))
                }
            }));

        let client = Client::new(service, config.default_namespace);

        std::env::set_var("MIRRORD_TEST_ENV_VAR_KUBECTL", "false");

        client.send(Request::new(Body::empty())).await.unwrap();
    }
}
