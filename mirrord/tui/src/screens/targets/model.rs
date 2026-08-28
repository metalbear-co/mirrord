//! Serializable model of the `mirrord-up.yaml` schema.
//!
//! The `mirrord-up` crate keeps its config fields `pub(crate)`, so this crate
//! cannot construct them directly. This module mirrors the schema instead,
//! and the round-trip tests below pin the shape we emit. Unlike `mirrord up`,
//! we do not use `deny_unknown_fields`: when loading a file that carries
//! settings this wizard does not know yet, we'd rather keep working than fail.

use std::collections::BTreeSet;

use serde::{
    Deserialize, Serialize,
    de::{MapAccess, Unexpected, Visitor, value::MapAccessDeserializer},
    ser::SerializeMap,
};
use strum::{Display, IntoStaticStr, VariantArray};

/// Incoming traffic mode for a service, mirroring `mirrord up`'s `ServiceMode`.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    PartialEq,
    Serialize,
    Deserialize,
    Display,
    IntoStaticStr,
    VariantArray,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum ServiceMode {
    /// Incoming traffic is split between local and cluster with an HTTP filter.
    #[default]
    Split,
    /// The local process takes over all traffic (copy target + scale down).
    Replace,
}

/// Whether the service runs with `mirrord exec` or `mirrord container`.
#[derive(
    Clone,
    Copy,
    Debug,
    Default,
    Eq,
    PartialEq,
    Serialize,
    Deserialize,
    Display,
    IntoStaticStr,
    VariantArray,
)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum RunType {
    #[default]
    Exec,
    Container,
}

impl RunType {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// The local command `mirrord up` launches for a service.
///
/// The wizard always runs services with `exec` (the `mirrord up` default),
/// so the `type` field is only written when a loaded file said otherwise.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunSpec {
    #[serde(rename = "type", default, skip_serializing_if = "RunType::is_default")]
    pub run_type: RunType,
    pub command: Vec<String>,
    /// Directory the command runs in; `None` runs from wherever
    /// `mirrord up` itself was started. TUI-side sugar only: `mirrord up`
    /// has no such field, so it never reaches the emitted file - the
    /// plan folds it into the command as a shell `cd` instead.
    #[serde(skip)]
    pub dir: Option<String>,
}

/// A service's target: explicitly targetless, or a target path.
///
/// Serializes exactly like `mirrord up`'s `TargetConfig`: the literal string
/// `none` for targetless, a `{ path, namespace }` mapping otherwise.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TargetSpec {
    Targetless,
    Path {
        /// A mirrord target path, e.g. `deployment/foo/container/bar`.
        path: String,
        namespace: Option<String>,
    },
}

impl TargetSpec {
    /// The target as shown to the user, e.g. `deployment/foo (staging)`.
    pub fn display(&self) -> String {
        match self {
            Self::Targetless => "targetless".to_owned(),
            Self::Path {
                path,
                namespace: Some(namespace),
            } => format!("{path} ({namespace})"),
            Self::Path {
                path,
                namespace: None,
            } => path.clone(),
        }
    }
}

#[derive(Serialize, Deserialize)]
struct TargetPath {
    path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    namespace: Option<String>,
}

impl Serialize for TargetSpec {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Targetless => serializer.serialize_str("none"),
            Self::Path { path, namespace } => TargetPath {
                path: path.clone(),
                namespace: namespace.clone(),
            }
            .serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for TargetSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct TargetVisitor;

        impl<'de> Visitor<'de> for TargetVisitor {
            type Value = TargetSpec;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("`none` or a `{ path, namespace }` mapping")
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if value == "none" {
                    Ok(TargetSpec::Targetless)
                } else {
                    Err(serde::de::Error::invalid_value(
                        Unexpected::Str(value),
                        &self,
                    ))
                }
            }

            fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
            where
                A: MapAccess<'de>,
            {
                let TargetPath { path, namespace } =
                    TargetPath::deserialize(MapAccessDeserializer::new(map))?;
                Ok(TargetSpec::Path { path, namespace })
            }
        }

        deserializer.deserialize_any(TargetVisitor)
    }
}

/// The HTTP filter subset the wizard exposes (header filter only for now).
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct HttpFilterSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header_filter: Option<String>,
}

/// Per-service configuration, mirroring `mirrord up`'s `ServiceConfig`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ServiceSpec {
    pub target: TargetSpec,
    #[serde(default)]
    pub default_mode: ServiceMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http_filter: Option<HttpFilterSpec>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub ignore_ports: BTreeSet<u16>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub skip: bool,
    pub run: RunSpec,
}

/// A named service in the plan. The name becomes the key in the
/// `services` mapping of the emitted file.
#[derive(Clone, Debug, PartialEq)]
pub struct ServiceEntry {
    pub name: String,
    pub spec: ServiceSpec,
}

/// Settings applied to all services, mirroring `mirrord up`'s `CommonConfig`.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommonSpec {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accept_invalid_certificates: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

impl CommonSpec {
    fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// The whole `mirrord-up.yaml` file.
///
/// Services are kept as a `Vec` (not a map) so the order the user arranged in
/// the plan pane is the order written to the file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UpFile {
    pub common: CommonSpec,
    pub services: Vec<ServiceEntry>,
}

impl UpFile {
    pub fn to_yaml(&self) -> anyhow::Result<String> {
        Ok(serde_yaml::to_string(self)?)
    }

    pub fn to_json(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "loading an existing plan file is not wired up yet"
        )
    )]
    pub fn from_yaml(source: &str) -> anyhow::Result<Self> {
        Ok(serde_yaml::from_str(source)?)
    }
}

impl Serialize for UpFile {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        struct Services<'a>(&'a [ServiceEntry]);

        impl Serialize for Services<'_> {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                let mut map = serializer.serialize_map(Some(self.0.len()))?;
                for entry in self.0 {
                    map.serialize_entry(&entry.name, &entry.spec)?;
                }
                map.end()
            }
        }

        let entries = 1 + usize::from(!self.common.is_default());
        let mut map = serializer.serialize_map(Some(entries))?;
        if !self.common.is_default() {
            map.serialize_entry("common", &self.common)?;
        }
        map.serialize_entry("services", &Services(&self.services))?;
        map.end()
    }
}

impl<'de> Deserialize<'de> for UpFile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        /// Deserializes the `services` mapping keeping the file's entry order.
        struct OrderedServices(Vec<ServiceEntry>);

        impl<'de> Deserialize<'de> for OrderedServices {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct ServicesVisitor;

                impl<'de> Visitor<'de> for ServicesVisitor {
                    type Value = OrderedServices;

                    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                        formatter.write_str("a mapping of service name to service config")
                    }

                    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
                    where
                        A: MapAccess<'de>,
                    {
                        let mut services = Vec::with_capacity(map.size_hint().unwrap_or(0));
                        while let Some((name, spec)) = map.next_entry::<String, ServiceSpec>()? {
                            services.push(ServiceEntry { name, spec });
                        }
                        Ok(OrderedServices(services))
                    }
                }

                deserializer.deserialize_map(ServicesVisitor)
            }
        }

        #[derive(Deserialize)]
        struct File {
            #[serde(default)]
            common: CommonSpec,
            services: OrderedServices,
        }

        let file = File::deserialize(deserializer)?;
        Ok(UpFile {
            common: file.common,
            services: file.services.0,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn service(path: &str, command: &[&str]) -> ServiceSpec {
        ServiceSpec {
            target: TargetSpec::Path {
                path: path.to_owned(),
                namespace: None,
            },
            default_mode: ServiceMode::Split,
            http_filter: None,
            ignore_ports: BTreeSet::new(),
            skip: false,
            run: RunSpec {
                run_type: RunType::Exec,
                command: command.iter().map(|part| (*part).to_owned()).collect(),
                dir: None,
            },
        }
    }

    #[test]
    fn round_trip_preserves_service_order() {
        let file = UpFile {
            common: CommonSpec::default(),
            services: vec![
                ServiceEntry {
                    name: "zebra".to_owned(),
                    spec: service("deployment/zebra", &["cargo", "run"]),
                },
                ServiceEntry {
                    name: "alpha".to_owned(),
                    spec: service("pod/alpha", &["npm", "start"]),
                },
                ServiceEntry {
                    name: "middle".to_owned(),
                    spec: service("statefulset/middle", &["go", "run", "."]),
                },
            ],
        };

        let yaml = file.to_yaml().unwrap();
        let zebra = yaml.find("zebra:").unwrap();
        let alpha = yaml.find("alpha:").unwrap();
        let middle = yaml.find("middle:").unwrap();
        assert!(
            zebra < alpha && alpha < middle,
            "emitted yaml lost order:\n{yaml}"
        );

        let parsed = UpFile::from_yaml(&yaml).unwrap();
        assert_eq!(parsed, file);
    }

    /// The example lives in `testdata/` so it reads as a real config file
    /// and can be tried directly with `mirrord up -f`.
    #[test]
    fn parses_the_documented_example() {
        let yaml = include_str!("testdata/mirrord-up-example.yaml");

        let file = UpFile::from_yaml(yaml).unwrap();
        assert_eq!(file.common.operator, Some(true));
        assert_eq!(file.services.len(), 2);

        let web = &file.services[0];
        assert_eq!(web.name, "web");
        assert_eq!(
            web.spec.target,
            TargetSpec::Path {
                path: "deployment/web-app".to_owned(),
                namespace: Some("staging".to_owned()),
            }
        );
        assert_eq!(web.spec.run.run_type, RunType::Container);
        assert_eq!(
            web.spec
                .http_filter
                .as_ref()
                .unwrap()
                .header_filter
                .as_deref(),
            Some("x-session: local"),
        );

        let api = &file.services[1];
        assert!(api.spec.skip);
        assert_eq!(api.spec.run.run_type, RunType::Exec);

        let round_tripped = UpFile::from_yaml(&file.to_yaml().unwrap()).unwrap();
        assert_eq!(round_tripped, file);
    }

    #[test]
    fn targetless_serializes_as_the_none_string() {
        let file = UpFile {
            common: CommonSpec::default(),
            services: vec![ServiceEntry {
                name: "local".to_owned(),
                spec: ServiceSpec {
                    target: TargetSpec::Targetless,
                    ..service("unused", &["make", "dev"])
                },
            }],
        };

        let yaml = file.to_yaml().unwrap();
        assert!(yaml.contains("target: none"), "unexpected yaml:\n{yaml}");
        assert_eq!(UpFile::from_yaml(&yaml).unwrap(), file);
    }

    /// `mirrord up` rejects unknown fields, so the TUI-only directory
    /// must never leak into the emitted yaml.
    #[test]
    fn run_dir_never_serializes() {
        let mut spec = service("deployment/svc", &["npm", "start"]);
        spec.run.dir = Some("/work/svc".to_owned());
        let file = UpFile {
            common: CommonSpec::default(),
            services: vec![ServiceEntry {
                name: "svc".to_owned(),
                spec,
            }],
        };
        assert!(
            !file.to_yaml().unwrap().contains("dir"),
            "unexpected yaml:\n{}",
            file.to_yaml().unwrap()
        );
    }

    #[test]
    fn default_common_is_omitted() {
        let file = UpFile {
            common: CommonSpec::default(),
            services: vec![ServiceEntry {
                name: "svc".to_owned(),
                spec: service("deployment/svc", &["true"]),
            }],
        };

        assert!(!file.to_yaml().unwrap().contains("common:"));
    }
}
