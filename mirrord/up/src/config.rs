use std::collections::{HashMap, HashSet};

use mirrord_config::{
    LayerConfig, LayerFileConfig,
    config::{ConfigContext, EnvKey, MirrordConfig},
    feature::{
        env::EnvConfig,
        network::incoming::{IncomingMode, http_filter::HttpFilterConfig},
    },
    target::Target,
};
use serde::{Deserialize, Serialize};
use strum_macros::IntoStaticStr;

/// Incoming traffic mode for a service.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ServiceMode {
    #[default]
    Split,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Default, IntoStaticStr)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum RunType {
    #[default]
    Exec,
    Container,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub struct RunConfig {
    #[serde(default)]
    pub r#type: RunType,
    pub command: Vec<String>,
}

/// Default settings applied to all services.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct DefaultsConfig {
    pub accept_invalid_certificates: Option<bool>,
    pub operator: Option<bool>,
    pub telemetry: bool,
}

impl Default for DefaultsConfig {
    fn default() -> Self {
        Self {
            accept_invalid_certificates: None,
            operator: None,
            telemetry: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, Default)]
pub struct TargetConfig {
    #[serde(deserialize_with = "mirrord_config::util::string_or_struct_option")]
    pub path: Option<Target>,
    pub namespace: Option<String>,
}

impl From<TargetConfig> for mirrord_config::target::TargetConfig {
    fn from(value: TargetConfig) -> Self {
        Self {
            path: value.path,
            namespace: value.namespace,
        }
    }
}

/// Per-service configuration.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct ServiceConfig {
    #[serde(default)]
    pub target: TargetConfig,

    #[serde(default)]
    pub env: EnvConfig,

    #[serde(default)]
    pub mode: ServiceMode,

    #[serde(default)]
    pub http_filter: HttpFilterConfig,

    #[serde(default)]
    pub ignore_ports: HashSet<u16>,

    pub run: RunConfig,
}

/// Resolved top-level `mirrord-up.yaml` configuration.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UpConfig {
    #[serde(default)]
    pub defaults: DefaultsConfig,
    pub services: HashMap<String, ServiceConfig>,
}

impl ServiceConfig {
    fn assemble(self, defaults: &DefaultsConfig, key: EnvKey) -> (LayerConfig, RunConfig) {
        let mut cfg = LayerFileConfig {
            accept_invalid_certificates: defaults.accept_invalid_certificates,
            operator: defaults.operator,
            telemetry: Some(defaults.telemetry),
            ..Default::default()
        }
        .generate_config(&mut ConfigContext::default())
        .unwrap();

        cfg.target = self.target.into();
        cfg.feature.env = self.env;

        match self.mode {
            ServiceMode::Split => {
                cfg.feature.network.incoming.mode = IncomingMode::Steal;

                cfg.feature.network.incoming.http_filter = if self.http_filter.is_filter_set() {
                    self.http_filter
                } else {
                    HttpFilterConfig {
                        header_filter: Some(format!(
                            "baggage: .*mirrord-session={}.*",
                            key.as_str()
                        )),
                        ..Default::default()
                    }
                };
            }
        }

        cfg.feature.network.incoming.ignore_ports = self.ignore_ports;
        cfg.key = key;

        (cfg, self.run)
    }
}

pub struct SubprocessCfg {
    pub config: LayerConfig,
    pub service_name: String,
    pub run: RunConfig,
}

impl UpConfig {
    pub fn service_configs<'a>(
        self,
        key: &'a EnvKey,
    ) -> impl Iterator<Item = SubprocessCfg> + use<'a> {
        let Self { defaults, services } = self;

        services.into_iter().map(move |(service_name, svc)| {
            let (config, run) = svc.assemble(&defaults, key.clone());
            SubprocessCfg {
                config,
                service_name,
                run,
            }
        })
    }
}

#[cfg(test)]
#[allow(clippy::indexing_slicing)]
mod tests {
    use super::*;

    /// Helper: parse YAML into UpConfig via the two-layer config system.
    fn parse(yaml: &str) -> UpConfig {
        serde_yaml::from_str(yaml).unwrap()
    }

    #[test]
    fn defaults_applied_when_omitted() {
        let config = parse(
            r#"
            services:
              my-svc:
                run:
                  command: ["echo"]
            "#,
        );
        assert_eq!(config.defaults, Default::default(),);
    }

    #[test]
    fn defaults_overridden() {
        let config = parse(
            r#"
            defaults:
              accept_invalid_certificates: true
              operator: false
              telemetry: false
            services:
              my-svc:
                run:
                  command: ["echo"]
            "#,
        );
        assert_eq!(
            config.defaults,
            DefaultsConfig {
                accept_invalid_certificates: Some(true),
                operator: Some(false),
                telemetry: false,
            }
        );
    }

    #[test]
    fn service_with_all_fields() {
        let config = parse(
            r#"
            services:
              web:
                target:
                  path: "deployment/web-app"
                  namespace: "staging"
                env:
                  override:
                    NODE_ENV: "development"
                mode: split
                http_filter:
                  header_filter: "x-session: local"
                run:
                  type: container
                  command: ["docker", "run", "-p", "8080:8080", "web:latest"]
            "#,
        );
        let svc = &config.services["web"];

        assert_eq!(
            svc.target.path.as_ref().unwrap().to_string(),
            "deployment/web-app"
        );
        assert_eq!(svc.target.namespace.as_deref(), Some("staging"));
        assert_eq!(
            svc.env.r#override.as_ref().unwrap()["NODE_ENV"],
            "development"
        );
        assert_eq!(svc.mode, ServiceMode::Split);
        assert_eq!(
            svc.http_filter.header_filter.as_deref(),
            Some("x-session: local")
        );
        assert_eq!(
            svc.run,
            RunConfig {
                r#type: RunType::Container,
                command: vec![
                    "docker".into(),
                    "run".into(),
                    "-p".into(),
                    "8080:8080".into(),
                    "web:latest".into(),
                ]
            }
        );
    }

    #[test]
    fn multiple_services_with_different_modes() {
        let config = parse(
            r#"
            services:
              svc-a:
                run:
                  type: exec
                  command: ["node", "a.js"]
              svc-b:
                run:
                  command: ["node", "b.js"]
            "#,
        );
        assert_eq!(config.services.len(), 2);

        for service in config.services.values() {
            assert_eq!(service.run.r#type, RunType::Exec);
        }
    }

    #[test]
    fn minimal_service_gets_defaults() {
        let config = parse(
            r#"
            services:
              svc:
                run:
                  command: ["echo"]
            "#,
        );
        let svc = &config.services["svc"];
        assert_eq!(svc.env, EnvConfig::default());
    }

    #[test]
    fn target_simple_string_form() {
        let config = parse(
            r#"
            services:
              svc:
                target:
                  path: "pod/my-pod/container/main"
                run:
                  command: ["echo"]
            "#,
        );
        assert_eq!(
            config.services["svc"]
                .target
                .path
                .as_ref()
                .unwrap()
                .to_string(),
            "pod/my-pod/container/main"
        );
    }

    // -- Error cases --

    #[test]
    fn error_missing_run() {
        let err = serde_yaml::from_str::<UpConfig>(
            r#"
            services:
              svc:
                mode: split
            "#,
        )
        .unwrap_err();
        assert!(
            err.to_string().contains("missing field `run`"),
            "expected error about missing run, got: {err}"
        );
    }

    #[test]
    fn error_invalid_mode() {
        let result: Result<UpConfig, _> = serde_yaml::from_str(
            r#"
            services:
              svc:
                mode: bogus
                run:
                  exec:
                    command: ["echo"]
            "#,
        );
        assert!(result.is_err());
    }
}
