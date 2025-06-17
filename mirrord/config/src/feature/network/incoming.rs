use std::{collections::HashSet, fmt, ops::Not, str::FromStr};

use bimap::BiMap;
use https_delivery::LocalHttpsDelivery;
use mirrord_analytics::{AnalyticValue, Analytics, CollectAnalytics};
use schemars::JsonSchema;
use serde::{de, ser, ser::SerializeSeq as _, Deserialize, Serialize};
use thiserror::Error;

use crate::{
    config::{
        from_env::FromEnv, source::MirrordConfigSource, unstable::Unstable, ConfigContext,
        ConfigError, FromMirrordConfig, MirrordConfig, Result,
    },
    util::{MirrordToggleableConfig, ToggleableConfig},
};

pub mod http_filter;
pub mod https_delivery;

use http_filter::*;

/// ## incoming (network)
///
/// Controls the incoming TCP traffic feature.
///
/// See the incoming [reference](https://metalbear.co/mirrord/docs/reference/traffic/#incoming) for more
/// details.
///
/// Incoming traffic supports 2 modes of operation:
///
/// 1. Mirror (**default**): Sniffs the TCP data from a port, and forwards a copy to the interested
///    listeners;
///
/// 2. Steal: Captures the TCP data from a port, and forwards it to the local process, see
///    [`steal`](##steal);
///
/// ### Minimal `incoming` config
///
/// ```json
/// {
///   "feature": {
///     "network": {
///       "incoming": "steal"
///     }
///   }
/// }
/// ```
///
/// ### Advanced `incoming` config
///
/// ```json
/// {
///   "feature": {
///     "network": {
///       "incoming": {
///         "mode": "steal",
///         "http_filter": {
///           "header_filter": "host: api\\..+"
///         },
///         "port_mapping": [[ 7777, 8888 ]],
///         "ignore_localhost": false,
///         "ignore_ports": [9999, 10000],
///         "listen_ports": [[80, 8111]]
///       }
///     }
///   }
/// }
/// ```
#[derive(Clone, Debug, JsonSchema)]
#[cfg_attr(test, derive(PartialEq, Eq))]
#[schemars(untagged, rename_all = "lowercase")]
pub enum IncomingFileConfig {
    Simple(Option<IncomingMode>),
    Advanced(Box<IncomingAdvancedFileConfig>),
}

impl Default for IncomingFileConfig {
    fn default() -> Self {
        IncomingFileConfig::Simple(None)
    }
}

impl FromMirrordConfig for IncomingConfig {
    type Generator = IncomingFileConfig;
}

impl MirrordConfig for IncomingFileConfig {
    type Generated = IncomingConfig;

    fn generate_config(self, context: &mut ConfigContext) -> Result<Self::Generated> {
        let config = match self {
            IncomingFileConfig::Simple(mode) => IncomingConfig {
                mode: FromEnv::new("MIRRORD_AGENT_TCP_STEAL_TRAFFIC")
                    .or(mode)
                    .source_value(context)
                    .transpose()?
                    .unwrap_or_default(),
                http_filter: HttpFilterFileConfig::default().generate_config(context)?,
                on_concurrent_steal: FromEnv::new("MIRRORD_OPERATOR_ON_CONCURRENT_STEAL")
                    .layer(|layer| Unstable::new("incoming", "on_concurrent_steal", layer))
                    .source_value(context)
                    .transpose()?
                    .unwrap_or_default(),
                ..Default::default()
            },
            IncomingFileConfig::Advanced(advanced) => IncomingConfig {
                mode: FromEnv::new("MIRRORD_AGENT_TCP_STEAL_TRAFFIC")
                    .or(advanced.mode)
                    .source_value(context)
                    .transpose()?
                    .unwrap_or_default(),
                http_filter: advanced
                    .http_filter
                    .unwrap_or_default()
                    .generate_config(context)?,
                port_mapping: advanced
                    .port_mapping
                    .map(|m| m.into_iter().collect())
                    .unwrap_or_default(),
                ignore_ports: advanced
                    .ignore_ports
                    .map(|m| m.into_iter().collect())
                    .unwrap_or_default(),
                ignore_localhost: advanced.ignore_localhost.unwrap_or_default(),
                listen_ports: advanced
                    .listen_ports
                    .map(|m| m.into_iter().collect())
                    .unwrap_or_default(),
                on_concurrent_steal: FromEnv::new("MIRRORD_OPERATOR_ON_CONCURRENT_STEAL")
                    .or(advanced.on_concurrent_steal)
                    .layer(|layer| Unstable::new("incoming", "on_concurrent_steal", layer))
                    .source_value(context)
                    .transpose()?
                    .unwrap_or_default(),
                ports: advanced.ports.map(|ports| ports.into_iter().collect()),
                https_delivery: advanced.https_delivery,
            },
        };

        Ok(config)
    }
}
impl MirrordToggleableConfig for IncomingFileConfig {
    fn disabled_config(context: &mut ConfigContext) -> Result<Self::Generated, ConfigError> {
        let mode = FromEnv::new("MIRRORD_AGENT_TCP_STEAL_TRAFFIC")
            .source_value(context)
            .unwrap_or_else(|| Ok(IncomingMode::Off))?;

        let on_concurrent_steal = FromEnv::new("MIRRORD_OPERATOR_ON_CONCURRENT_STEAL")
            .layer(|layer| Unstable::new("incoming", "on_concurrent_steal", layer))
            .source_value(context)
            .transpose()?
            .unwrap_or_default();

        Ok(IncomingConfig {
            mode,
            on_concurrent_steal,
            http_filter: HttpFilterFileConfig::disabled_config(context)?,
            ..Default::default()
        })
    }
}

// Change to manual deserializtion to prevent usless untagged enum errors
impl<'de> Deserialize<'de> for IncomingFileConfig {
    fn deserialize<D>(deserializer: D) -> Result<IncomingFileConfig, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        deserializer.deserialize_any(IncomingFileConfigVisitor)
    }
}

/// [`Visitor`](de::Visitor) for [`IncomingFileConfig`] that searches for bool or string for
/// `IncomingFileConfig::Simple` and map for `IncomingAdvancedFileConfig` directly
struct IncomingFileConfigVisitor;

impl<'de> de::Visitor<'de> for IncomingFileConfigVisitor {
    type Value = IncomingFileConfig;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("bool or string or map")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        let mode = if value {
            IncomingMode::default()
        } else {
            IncomingMode::Off
        };
        Ok(IncomingFileConfig::Simple(Some(mode)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Ok(IncomingFileConfig::Simple(None))
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: de::Deserializer<'de>,
    {
        Option::deserialize(deserializer).map(IncomingFileConfig::Simple)
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Deserialize::deserialize(de::value::StrDeserializer::new(value))
            .map(Some)
            .map(IncomingFileConfig::Simple)
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        Deserialize::deserialize(de::value::StringDeserializer::new(value))
            .map(Some)
            .map(IncomingFileConfig::Simple)
    }

    fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
    where
        M: de::MapAccess<'de>,
    {
        Deserialize::deserialize(de::value::MapAccessDeserializer::new(map))
            .map(IncomingFileConfig::Advanced)
    }
}

/// ## incoming (advanced setup)
///
/// Advanced user configuration for network incoming traffic.
#[derive(Deserialize, Clone, Debug, JsonSchema)]
#[cfg_attr(test, derive(PartialEq, Eq))]
#[serde(deny_unknown_fields)]
pub struct IncomingAdvancedFileConfig {
    /// ### mode
    ///
    /// Allows selecting between mirrorring or stealing traffic.
    ///
    /// See [`mode`](##mode (incoming)) for details.
    pub mode: Option<IncomingMode>,

    /// ### HTTP Filter
    ///
    /// Sets up the HTTP traffic filter (currently, only useful when `incoming: steal`).
    ///
    /// See [`filter`](##filter) for details.
    pub http_filter: Option<ToggleableConfig<http_filter::HttpFilterFileConfig>>,

    /// ### port_mapping
    ///
    /// Mapping for local ports to remote ports.
    ///
    /// This is useful when you want to mirror/steal a port to a different port on the remote
    /// machine. For example, your local process listens on port `9333` and the container listens
    /// on port `80`. You'd use `[[9333, 80]]`
    pub port_mapping: Option<Vec<(u16, u16)>>,

    /// ### ignore_localhost
    ///
    /// Consider removing when adding <https://github.com/metalbear-co/mirrord/issues/702>
    pub ignore_localhost: Option<bool>,

    /// ### ignore_ports
    ///
    /// Ports to ignore when mirroring/stealing traffic. Useful if you want specific ports to be
    /// used locally only.
    ///
    /// Mutually exclusive with [`ports`](###ports).
    pub ignore_ports: Option<Vec<u16>>,

    /// ### listen_ports
    ///
    /// Mapping for local ports to actually used local ports.
    /// When application listens on a port while steal/mirror is active
    /// we fallback to random ports to avoid port conflicts.
    /// Using this configuration will always use the specified port.
    /// If this configuration doesn't exist, mirrord will try to listen on the original port
    /// and if it fails it will assign a random port
    ///
    /// This is useful when you want to access ports exposed by your service locally
    /// For example, if you have a service that listens on port `80` and you want to access it,
    /// you probably can't listen on `80` without sudo, so you can use `[[80, 4480]]`
    /// then access it on `4480` while getting traffic from remote `80`.
    /// The value of `port_mapping` doesn't affect this.
    pub listen_ports: Option<Vec<(u16, u16)>>,

    /// ### on_concurrent_steal
    ///
    /// (Operator Only): if value of override will force close any other connections on requested
    /// target
    pub on_concurrent_steal: Option<ConcurrentSteal>,

    /// ### ports
    ///
    /// List of ports to mirror/steal traffic from. Other ports will remain local.
    ///
    /// Mutually exclusive with [`ignore_ports`](###ignore_ports).
    pub ports: Option<Vec<u16>>,

    /// ### https_delivery
    ///
    /// (Operator Only): configures how mirrord delivers stolen HTTPS requests
    /// to the local application.
    #[serde(default)]
    pub https_delivery: LocalHttpsDelivery,
}

fn serialize_bi_map<S>(map: &BiMap<u16, u16>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: ser::Serializer,
{
    // NB: this serialises the BiMap to a vec
    let mut seq = serializer.serialize_seq(Some(map.len()))?;

    for (key, value) in map {
        seq.serialize_element(&[key, value])?;
    }

    seq.end()
}

fn deserialize_bi_map<'de, D>(deserializer: D) -> Result<BiMap<u16, u16>, D::Error>
where
    D: de::Deserializer<'de>,
{
    // NB: this deserialises the BiMap from a vec
    let vec: Vec<(u16, u16)> = Vec::deserialize(deserializer)?;

    let mut elements = BiMap::new();
    vec.iter().for_each(|(key, value)| {
        elements.insert(*key, *value);
    });
    Ok(elements)
}

/// Controls the incoming TCP traffic feature.
///
/// See the incoming [reference](https://metalbear.co/mirrord/docs/reference/traffic/#incoming) for more
/// details.
///
/// Incoming traffic supports 3 [modes](#feature-network-incoming-mode) of operation:
///
/// 1. Mirror (**default**): Sniffs the TCP data from a port, and forwards a copy to the interested
///    listeners;
///
/// 2. Steal: Captures the TCP data from a port, and forwards it to the local process.
///
/// 3. Off: Disables the incoming network feature.
///
/// This field can either take an object with more configuration fields (that are documented below),
/// or alternatively -
/// - A boolean:
///   - `true`: use the default configuration, same as not specifying this field at all.
///   - `false`: disable incoming configuration.
/// - One of the incoming [modes](#feature-network-incoming-mode) (lowercase).
///
/// Examples:
///
/// Steal all the incoming traffic:
///
/// ```json
/// {
///   "feature": {
///     "network": {
///       "incoming": "steal"
///     }
///   }
/// }
/// ```
///
/// Disable the incoming traffic feature:
///
/// ```json
/// {
///   "feature": {
///     "network": {
///       "incoming": false
///     }
///   }
/// }
/// ```
///
/// Steal only traffic that matches the
/// [`http_filter`](#feature-network-incoming-http_filter) (steals only HTTP traffic).
///
/// ```json
/// {
///   "feature": {
///     "network": {
///       "incoming": {
///         "mode": "steal",
///         "http_filter": {
///           "header_filter": "host: api\\..+"
///         },
///         "port_mapping": [[ 7777, 8888 ]],
///         "ignore_localhost": false,
///         "ignore_ports": [9999, 10000],
///         "listen_ports": [[80, 8111]]
///       }
///     }
///   }
/// }
/// ```
#[derive(Default, PartialEq, Eq, Clone, Debug, Serialize, Deserialize)]
pub struct IncomingConfig {
    /// #### feature.network.incoming.port_mapping {#feature-network-incoming-port_mapping}
    ///
    /// Mapping for local ports to remote ports.
    ///
    /// This is useful when you want to mirror/steal a port to a different port on the remote
    /// machine. For example, your local process listens on port `9333` and the container listens
    /// on port `80`. You'd use `[[9333, 80]]`
    #[serde(
        serialize_with = "serialize_bi_map",
        deserialize_with = "deserialize_bi_map"
    )]
    pub port_mapping: BiMap<u16, u16>,

    /// #### feature.network.incoming.ignore_localhost {#feature-network-incoming-ignore_localhost}
    pub ignore_localhost: bool,

    /// #### feature.network.incoming.ignore_ports {#feature-network-incoming-ignore_ports}
    ///
    /// Ports to ignore when mirroring/stealing traffic, these ports will remain local.
    ///
    /// Can be especially useful when
    /// [`feature.network.incoming.mode`](#feature-network-incoming-mode) is set to `"steal"`,
    /// and you want to avoid redirecting traffic from some ports (for example, traffic from
    /// a health probe, or other heartbeat-like traffic).
    ///
    /// Mutually exclusive with [`feature.network.incoming.ports`](#feature-network-ports).
    pub ignore_ports: HashSet<u16>,

    /// #### feature.network.incoming.mode {#feature-network-incoming-mode}
    pub mode: IncomingMode,

    /// #### feature.network.incoming.http_filter {#feature-network-incoming-http-filter}
    pub http_filter: HttpFilterConfig,

    /// #### feature.network.incoming.listen_ports {#feature-network-incoming-listen_ports}
    ///
    /// Mapping for local ports to actually used local ports.
    /// When application listens on a port while steal/mirror is active
    /// we fallback to random ports to avoid port conflicts.
    /// Using this configuration will always use the specified port.
    /// If this configuration doesn't exist, mirrord will try to listen on the original port
    /// and if it fails it will assign a random port
    ///
    /// This is useful when you want to access ports exposed by your service locally
    /// For example, if you have a service that listens on port `80` and you want to access it,
    /// you probably can't listen on `80` without sudo, so you can use `[[80, 4480]]`
    /// then access it on `4480` while getting traffic from remote `80`.
    /// The value of `port_mapping` doesn't affect this.
    #[serde(
        serialize_with = "serialize_bi_map",
        deserialize_with = "deserialize_bi_map"
    )]
    pub listen_ports: BiMap<u16, u16>,

    /// #### feature.network.incoming.on_concurrent_steal {#feature-network-incoming-on_concurrent_steal}
    pub on_concurrent_steal: ConcurrentSteal,

    /// #### feature.network.incoming.ports {#feature-network-incoming-ports}
    ///
    /// List of ports to mirror/steal traffic from. Other ports will remain local.
    ///
    /// Mutually exclusive with
    /// [`feature.network.incoming.ignore_ports`](#feature-network-ignore_ports).
    pub ports: Option<HashSet<u16>>,

    /// #### feature.network.incoming.https_delivery {#feature-network-incoming-https_delivery}
    ///
    /// (Operator Only): configures how mirrord delivers stolen HTTPS requests
    /// to the local application.
    #[serde(default)]
    pub https_delivery: LocalHttpsDelivery,
}

impl IncomingConfig {
    /// <!--${internal}-->
    /// Helper function.
    ///
    /// Used by mirrord-layer to identify the incoming network configuration as steal or not.
    pub fn is_steal(&self) -> bool {
        matches!(self.mode, IncomingMode::Steal)
    }

    /// <!--${internal}-->
    /// Helper function
    ///
    /// Checks whether the given port can be stolen without an HTTP filter.
    pub fn steals_port_without_filter(&self, port: u16) -> bool {
        if self.is_steal().not() {
            return false;
        }

        if self.http_filter.is_filter_set() {
            if let Some(filter_ports) = self.http_filter.ports.as_ref()
                && filter_ports.contains(&port)
            {
                false
            } else {
                self.ports.as_ref().is_some_and(|set| set.contains(&port))
            }
        } else if self.ignore_ports.contains(&port) {
            false
        } else if let Some(ports) = &self.ports {
            ports.contains(&port)
        } else {
            true
        }
    }

    /// Update the [`HttpFilterConfig::ports`] with the health probes ports from the target and
    /// ports `[80, 8080]`.
    ///
    /// Usually the user app will be listening on HTTP on the same ports as these probes, so
    /// we can insert them in the user config.
    ///
    /// If the user has set anything in [`HttpFilterConfig::ports`], then we do nothing, to
    /// avoid overriding their config. We also take care to not create conflicts with other
    /// port configs that we have, such as [`IncomingConfig::ignore_ports`]`, and
    /// [`IncomingConfig::ports`].
    pub fn add_probe_ports_to_http_filter_ports(
        &mut self,
        probes_ports: &[u16],
    ) -> Option<&PortList> {
        if self.is_steal() && self.http_filter.is_filter_set() && self.http_filter.ports.is_none() {
            let filtered_ports = probes_ports
                .iter()
                .chain(&[80, 8080])
                // Avoid conflicts with `incoming.ignore_ports`.
                .filter(|port| self.ignore_ports.contains(port).not())
                .filter(|port| {
                    // Avoid conflicts with `incoming.ports`.
                    if let Some(ports) = &self.ports {
                        ports.contains(port).not()
                    } else {
                        true
                    }
                })
                .copied()
                .collect::<HashSet<_>>();

            // Only add something if we have a port to add, otherwise leave it as `None` so
            // we can use the `PortList::default` when initializing things.
            if filtered_ports.is_empty().not() {
                self.http_filter.ports.replace(filtered_ports.into());
            }
        }

        self.http_filter.ports.as_ref()
    }
}

/// Allows selecting between mirrorring or stealing traffic.
///
/// Can be set to either `"mirror"` (default), `"steal"` or `"off"`.
///
/// - `"mirror"`: Sniffs on TCP port, and send a copy of the data to listeners.
/// - `"off"`: Disables the incoming network feature.
/// - `"steal"`: Supports 2 modes of operation:
///
/// 1. Port traffic stealing: Steals all TCP data from a port, which is selected whenever the user
///    listens in a TCP socket (enabling the feature is enough to make this work, no additional
///    configuration is needed);
///
/// 2. HTTP traffic stealing: Steals only HTTP traffic, mirrord tries to detect if the incoming data
///    on a port is HTTP (in a best-effort kind of way, not guaranteed to be HTTP), and steals the
///    traffic on the port if it is HTTP;
#[derive(Serialize, Deserialize, PartialEq, Eq, Clone, Copy, Debug, JsonSchema, Default)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub enum IncomingMode {
    /// <!--${internal}-->
    /// ### mirror
    ///
    /// Sniffs on TCP port, and send a copy of the data to listeners.
    #[default]
    Mirror,

    /// <!--${internal}-->
    /// ### steal
    ///
    /// Stealer supports 2 modes of operation:
    ///
    /// 1. Port traffic stealing: Steals all TCP data from a port, which is selected whenever the
    ///    user listens in a TCP socket (enabling the feature is enough to make this work, no
    ///    additional configuration is needed);
    ///
    /// 2. HTTP traffic stealing: Steals only HTTP traffic, mirrord tries to detect if the incoming
    ///    data on a port is HTTP (in a best-effort kind of way, not guaranteed to be HTTP), and
    ///    steals the traffic on the port if it is HTTP;
    Steal,

    /// <!--${internal}-->
    /// ### Off
    ///
    /// Disables the incoming network feature.
    Off,
}

#[derive(Error, Debug)]
#[error("could not parse IncomingConfig from string, values must be bool or mirror/steal")]
pub struct IncomingConfigParseError;

impl FromStr for IncomingMode {
    type Err = IncomingConfigParseError;

    fn from_str(val: &str) -> Result<Self, Self::Err> {
        match val.parse::<bool>() {
            Ok(true) => Ok(Self::Steal),
            Ok(false) => Ok(Self::Off),
            Err(_) => match val {
                "steal" => Ok(Self::Steal),
                "mirror" => Ok(Self::Mirror),
                "off" => Ok(Self::Off),
                _ => Err(IncomingConfigParseError),
            },
        }
    }
}

/// (Operator Only): Allows overriding port locks
///
/// Can be set to either `"continue"` or `"override"`.
///
/// - `"continue"`: Continue with normal execution
/// - `"override"`: If port lock detected then override it with new lock and force close the
///   original locking connection.
#[derive(Default, Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "lowercase")]
pub enum ConcurrentSteal {
    /// <!--${internal}-->
    /// ### override
    ///
    /// Override any port lock and force close the original lock connection
    Override,
    /// <!--${internal}-->
    /// ### continue
    ///
    /// Continue with normal execution
    Continue,
    /// <!--${internal}-->
    /// ### abort
    ///
    /// Abort Execution when trying to steal traffic from a target whose traffic is already being
    /// stolen.
    #[default]
    Abort,
}

#[derive(Error, Debug)]
#[error("could not parse ConcurrentSteal from string, values continue/override")]
pub struct ConcurrentStealParseError;

impl FromStr for ConcurrentSteal {
    type Err = ConcurrentStealParseError;

    fn from_str(val: &str) -> Result<Self, Self::Err> {
        match val {
            "abort" => Ok(Self::Abort),
            "continue" => Ok(Self::Continue),
            "override" => Ok(Self::Override),
            _ => Err(ConcurrentStealParseError),
        }
    }
}

impl fmt::Display for ConcurrentSteal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Abort => write!(f, "abort"),
            Self::Continue => write!(f, "continue"),
            Self::Override => write!(f, "override"),
        }
    }
}

impl From<&IncomingMode> for AnalyticValue {
    fn from(value: &IncomingMode) -> Self {
        match value {
            IncomingMode::Mirror => AnalyticValue::Number(0),
            IncomingMode::Steal => AnalyticValue::Number(1),
            IncomingMode::Off => AnalyticValue::Number(2),
        }
    }
}

impl From<&ConcurrentSteal> for AnalyticValue {
    fn from(value: &ConcurrentSteal) -> Self {
        match value {
            ConcurrentSteal::Override => AnalyticValue::Number(0),
            ConcurrentSteal::Continue => AnalyticValue::Number(1),
            ConcurrentSteal::Abort => AnalyticValue::Number(2),
        }
    }
}

impl CollectAnalytics for &IncomingConfig {
    fn collect_analytics(&self, analytics: &mut Analytics) {
        analytics.add("mode", &self.mode);
        analytics.add("concurrent_steal", &self.on_concurrent_steal);
        analytics.add("port_mapping_count", self.port_mapping.len());
        analytics.add("listen_ports_count", self.listen_ports.len());
        analytics.add("ignore_localhost", self.ignore_localhost);
        analytics.add("ignore_ports_count", self.ignore_ports.len());
        analytics.add("http", &self.http_filter);
    }
}

#[cfg(test)]
mod test {
    use rstest::rstest;

    use super::IncomingConfig;
    use crate::feature::network::incoming::{http_filter::HttpFilterConfig, IncomingMode};

    #[rstest]
    #[case(
        IncomingConfig {
            mode: IncomingMode::Steal,
            http_filter: HttpFilterConfig {
                header_filter: Some("x-user: me".into()),
                ..Default::default()
            },
            ..Default::default()
        },
        80,
        false,
    )]
    #[case(
        IncomingConfig {
            mode: IncomingMode::Steal,
            http_filter: HttpFilterConfig {
                header_filter: Some("x-user: me".into()),
                ..Default::default()
            },
            ..Default::default()
        },
        81,
        false,
    )]
    #[case(
        IncomingConfig {
            mode: IncomingMode::Steal,
            http_filter: HttpFilterConfig {
                header_filter: Some("x-user: me".into()),
                ..Default::default()
            },
            ports: Some([81].into()),
            ..Default::default()
        },
        81,
        true,
    )]
    #[case(
        IncomingConfig {
            mode: IncomingMode::Mirror,
            ..Default::default()
        },
        80,
        false,
    )]
    #[case(
        IncomingConfig {
            mode: IncomingMode::Off,
            ..Default::default()
        },
        80,
        false,
    )]
    #[case(
        IncomingConfig {
            mode: IncomingMode::Steal,
            ..Default::default()
        },
        80,
        true,
    )]
    #[case(
        IncomingConfig {
            mode: IncomingMode::Steal,
            ignore_ports: [80].into(),
            ..Default::default()
        },
        80,
        false,
    )]
    #[case(
        IncomingConfig {
            mode: IncomingMode::Steal,
            ports: Some([80].into()),
            ..Default::default()
        },
        81,
        false,
    )]
    #[test]
    fn steals_port_without_filter(
        #[case] config: IncomingConfig,
        #[case] port: u16,
        #[case] expected: bool,
    ) {
        let result = config.steals_port_without_filter(port);
        assert_eq!(result, expected);
    }

    #[rstest]
    // case_1: Base case, with default ports 80 and 8080 added. Port 80 is filtered out due to
    // conflict with incoming.ports.
    #[case(
        IncomingConfig {
            mode: IncomingMode::Steal,
            ports: Some([80].into()),
            http_filter: HttpFilterConfig {
                header_filter: Some("siemowit".into()),
                ..Default::default()
            },
            ..Default::default()
        },
        81,
        IncomingConfig {
            mode: IncomingMode::Steal,
            ports: Some([80].into()),
            http_filter: HttpFilterConfig {
                header_filter: Some("siemowit".into()),
                ports: Some(vec![81, 8080].into()),
                ..Default::default()
            },
            ..Default::default()
        }
    )]
    // case_2: User sets `HttpFilter::ports`, we don't change it.
    #[case(
        IncomingConfig {
            mode: IncomingMode::Steal,
            ports: Some([80].into()),
            http_filter: HttpFilterConfig {
                header_filter: Some("lestek".into()),
                ports: Some(vec![82].into()),
                ..Default::default()
            },
            ..Default::default()
        },
        81,
        IncomingConfig {
            mode: IncomingMode::Steal,
            ports: Some([80].into()),
            http_filter: HttpFilterConfig {
                header_filter: Some("lestek".into()),
                ports: Some(vec![82].into()),
                ..Default::default()
            },
            ..Default::default()
        }
    )]
    // case_3: Conflicts between `IncomingConfig::ports` and probe port, but default ports 80 and
    // 8080 are still added.
    #[case(
        IncomingConfig {
            mode: IncomingMode::Steal,
            ports: Some([81].into()),
            http_filter: HttpFilterConfig {
                header_filter: Some("siemomysł".into()),
                ..Default::default()
            },
            ..Default::default()
        },
        81,
        IncomingConfig {
            mode: IncomingMode::Steal,
            ports: Some([81].into()),
            http_filter: HttpFilterConfig {
                header_filter: Some("siemomysł".into()),
                ports: Some(vec![80, 8080].into()),
                ..Default::default()
            },
            ..Default::default()
        }
    )]
    // case_4: Conflicts between `IncomingConfig::ignore_ports` and probe port, but default ports 80
    // and 8080 are still added.
    #[case(
        IncomingConfig {
            mode: IncomingMode::Steal,
            ports: Some([80].into()),
            ignore_ports: [81].into(),
            http_filter: HttpFilterConfig {
                header_filter: Some("otto".into()),
                ..Default::default()
            },
            ..Default::default()
        },
        81,
        IncomingConfig {
            mode: IncomingMode::Steal,
            ports: Some([80].into()),
            ignore_ports: [81].into(),
            http_filter: HttpFilterConfig {
                header_filter: Some("otto".into()),
                ports: Some(vec![8080].into()),
                ..Default::default()
            },
            ..Default::default()
        }
    )]
    #[test]
    /// Validates that we don't create conflicting configs between [`IncomingConfig`] _port_ related
    /// configs and [`HttpFilterConfig::ports`].
    fn automatically_add_probes_to_http_filter_ports(
        #[case] mut config: IncomingConfig,
        #[case] port: u16,
        #[case] expected: IncomingConfig,
    ) {
        config.add_probe_ports_to_http_filter_ports(&[port]);

        // Sort the ports since `HashSet` does not guarantee order.
        if let Some(http_filter_ports) = config.http_filter.ports.as_mut() {
            let mut ports_vec: Vec<u16> = http_filter_ports.clone().into();
            ports_vec.sort();
            *http_filter_ports = ports_vec.into();
        }

        assert_eq!(config, expected);
    }
}
