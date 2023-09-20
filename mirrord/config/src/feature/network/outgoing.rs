use core::str::FromStr;
use std::net::SocketAddr;

use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::Deserialize;
use thiserror::Error;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigContext, ConfigError},
    util::{MirrordToggleableConfig, VecOrSingle},
};

/// List of addresses/ports/subnets that should be sent through either the remote pod or local app,
/// depending how you set this up with either `remote` or `local`.
///
/// You may use this option to specify when outgoing traffic is sent from the remote pod (which
/// is the default behavior when you enable outgoing traffic), or from the local app (default when
/// you have outgoing traffic disabled).
///
/// Takes a list of values, such as:
///
/// - Only UDP traffic on subnet `1.1.1.0/24` on port 1337 will go through the remote pod.
///
/// ```json
/// {
///   "remote": ["udp://1.1.1.0/24:1337"]
/// }
/// ```
///
/// - Only UDP and TCP traffic on resolved address of `google.com` on port `1337` and `7331`
/// will go through the remote pod.
/// ```json
/// {
///   "remote": ["google.com:1337", "google.com:7331"]
/// }
/// ```
///
/// - Only TCP traffic on `localhost` on port 1337 will go through the local app, the rest will be
///   emmited remotely in the cluster.
///
/// ```json
/// {
///   "local": ["tcp://localhost:1337"]
/// }
/// ```
///
/// - Only outgoing traffic on port `1337` and `7331` will go through the local app.
/// ```json
/// {
///   "local": [":1337", ":7331"]
/// }
/// ```
///
/// Valid values follow this pattern: `[protocol]://[name|address|subnet/mask]:[port]`.
#[derive(Deserialize, PartialEq, Eq, Clone, Debug, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum OutgoingFilterConfig {
    /// Traffic that matches what's specified here will go through the remote pod, everything else
    /// will go through local.
    Remote(VecOrSingle<String>),

    /// Traffic that matches what's specified here will go through the local app, everything else
    /// will go through the remote pod.
    Local(VecOrSingle<String>),
}

/// Tunnel outgoing network operations through mirrord.
///
/// See the outgoing [reference](https://mirrord.dev/docs/reference/traffic/#outgoing) for more
/// details.
///
/// The `remote` and `local` config for this feature are **mutually** exclusive.
///
/// ```json
/// {
///   "feature": {
///     "network": {
///       "outgoing": {
///         "tcp": true,
///         "udp": true,
///         "ignore_localhost": false,
///         "filter": {
///           "local": ["tcp://1.1.1.0/24:1337", "1.1.5.0/24", "google.com", ":53"]
///         },
///         "unix_streams": "bear.+"
///       }
///     }
///   }
/// }
/// ```
#[derive(MirrordConfig, Default, PartialEq, Eq, Clone, Debug)]
#[config(map_to = "OutgoingFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq, Eq"))]
pub struct OutgoingConfig {
    /// #### feature.network.outgoing.tcp {#feature.network.outgoing.tcp}
    ///
    /// Defaults to `true`.
    #[config(env = "MIRRORD_TCP_OUTGOING", default = true)]
    pub tcp: bool,

    /// #### feature.network.outgoing.udp {#feature.network.outgoing.udp}
    ///
    /// Defaults to `true`.
    #[config(env = "MIRRORD_UDP_OUTGOING", default = true)]
    pub udp: bool,

    /// #### feature.network.outgoing.ignore_localhost {#feature.network.outgoing.ignore_localhost}
    ///
    /// Defaults to `false`.
    // Consider removing when adding https://github.com/metalbear-co/mirrord/issues/702
    #[config(unstable, default = false)]
    pub ignore_localhost: bool,

    /// #### feature.network.outgoing.filter {#feature.network.outgoing.filter}
    ///
    /// Unstable: the precise syntax of this config is subject to change.
    #[config(default, unstable)]
    pub filter: Option<OutgoingFilterConfig>,

    /// #### feature.network.outgoing.unix_streams {#feature.network.outgoing.unix_streams}
    ///
    /// Connect to these unix streams remotely (and to all other paths locally).
    ///
    /// You can either specify a single value or an array of values.
    /// Each value is interpreted as a regular expression
    /// ([Supported Syntax](https://docs.rs/regex/1.7.1/regex/index.html#syntax)).
    ///
    /// When your application connects to a unix socket, the target address will be converted to a
    /// string (non-utf8 bytes are replaced by a placeholder character) and matched against the set
    /// of regexes specified here. If there is a match, mirrord will connect your application with
    /// the target unix socket address on the target pod. Otherwise, it will leave the connection
    /// to happen locally on your machine.
    #[config(unstable, env = "MIRRORD_OUTGOING_REMOTE_UNIX_STREAMS")]
    pub unix_streams: Option<VecOrSingle<String>>,
}

impl MirrordToggleableConfig for OutgoingFileConfig {
    fn disabled_config(context: &mut ConfigContext) -> Result<Self::Generated, ConfigError> {
        Ok(OutgoingConfig {
            tcp: FromEnv::new("MIRRORD_TCP_OUTGOING")
                .source_value(context)
                .unwrap_or(Ok(false))?,
            udp: FromEnv::new("MIRRORD_UDP_OUTGOING")
                .source_value(context)
                .unwrap_or(Ok(false))?,
            unix_streams: FromEnv::new("MIRRORD_OUTGOING_REMOTE_UNIX_STREAMS")
                .source_value(context)
                .transpose()?,
            ..Default::default()
        })
    }
}

/// <!--${internal}-->
/// Errors related to parsing an [`OutgoingFilter`].
#[derive(Debug, Error)]
pub enum OutgoingFilterError {
    #[error("Nom: failed parsing with {0}!")]
    Nom2(nom::Err<nom::error::Error<String>>),

    #[error("Subnet: Failed parsing with {0}!")]
    Subnet(#[from] ipnet::AddrParseError),

    #[error("ParseInt: Failed converting string into `u16` with {0}!")]
    ParseInt(#[from] std::num::ParseIntError),

    #[error("Failed parsing protocol value of {0}!")]
    InvalidProtocol(String),

    #[error("Found trailing value after parsing {0}!")]
    TrailingValue(String),
}

impl From<nom::Err<nom::error::Error<&str>>> for OutgoingFilterError {
    fn from(value: nom::Err<nom::error::Error<&str>>) -> Self {
        Self::Nom2(value.to_owned())
    }
}

/// <!--${internal}-->
/// The protocols we support on [`OutgoingFilter`].
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProtocolFilter {
    #[default]
    Any,
    Tcp,
    Udp,
}

impl FromStr for ProtocolFilter {
    type Err = OutgoingFilterError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lowercase = s.to_lowercase();

        match lowercase.as_str() {
            "any" => Ok(Self::Any),
            "tcp" => Ok(Self::Tcp),
            "udp" => Ok(Self::Udp),
            invalid => Err(OutgoingFilterError::InvalidProtocol(invalid.to_string())),
        }
    }
}

/// <!--${internal}-->
/// Parsed addresses can be one of these 3 variants.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AddressFilter {
    /// Just a plain old [`SocketAddr`], specified as `a.b.c.d:e`.
    ///
    /// We treat `0`s here as if it meant **any**, so `0.0.0.0` means we filter any IP, and `:0`
    /// means any port.
    Socket(SocketAddr),

    /// A named address, as we cannot resolve it here, specified as `name:a`.
    ///
    /// We can only resolve such names on the mirrord layer `connect` call, as we have to check if
    /// the user enabled the DNS feature or not (and thus, resolve it through the remote pod, or
    /// the local app).
    Name((String, u16)),

    /// Just a plain old subnet and a port, specified as `a.b.c.d/e:f`.
    Subnet((ipnet::IpNet, u16)),
}

/// <!--${internal}-->
/// The parsed filter with its [`ProtocolFilter`] and [`AddressFilter`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OutgoingFilter {
    /// Valid protocol types.
    pub protocol: ProtocolFilter,

    /// Address|name|subnet we're going to filter.
    pub address: AddressFilter,
}

/// <!--${internal}-->
/// It's dangerous to go alone!
/// Take [this](https://github.com/rust-bakery/nom/blob/main/doc/choosing_a_combinator.md).
///
/// [`nom`] works better with `u8` slices, instead of `str`s.
mod parser {
    use nom::{
        branch::alt,
        bytes::complete::{tag, take_until},
        character::complete::{alphanumeric1, digit1},
        combinator::opt,
        multi::many1,
        sequence::{delimited, preceded, terminated},
        IResult,
    };

    /// <!--${internal}-->
    ///
    /// Parses `tcp://`, extracting the `tcp` part, and discarding the `://`.
    pub(super) fn protocol(input: &str) -> IResult<&str, &str> {
        let (rest, protocol) = opt(terminated(take_until("://"), tag("://")))(input)?;
        let protocol = protocol.unwrap_or("any");

        Ok((rest, protocol))
    }

    /// <!--${internal}-->
    ///
    /// We try to parse 3 different kinds of values here:
    ///
    /// 1. `name.with.dots`;
    /// 2. `1.2.3.4.5.6`;
    /// 3. `[dad:1337:fa57::0]`
    ///
    /// Where 1 and 2 are handled by `dotted_address`.
    ///
    /// The parser is not interested in only eating correct values here for hostnames, ip addresses,
    /// etc., it just tries to get a good enough string that could be parsed by
    /// `SocketAddr::parse`, or `IpNet::parse`.
    ///
    /// Returns `0.0.0.0` if it doesn't parse anything.
    pub(super) fn address(input: &str) -> IResult<&str, String> {
        let ipv6 = many1(alt((alphanumeric1, tag(":"))));
        let ipv6_host = delimited(tag("["), ipv6, tag("]"));

        let host_char = alt((alphanumeric1, tag("-"), tag("_"), tag(".")));
        let dotted_address = many1(host_char);

        let (rest, address) = opt(alt((dotted_address, ipv6_host)))(input)?;

        let address = address
            .map(|addr| addr.concat())
            .unwrap_or(String::from("0.0.0.0"));

        Ok((rest, address))
    }

    /// <!--${internal}-->
    ///
    /// Parses `/24`, extracting the `24` part, and discarding the `/`.
    pub(super) fn subnet(input: &str) -> IResult<&str, Option<&str>> {
        let subnet_parser = preceded(tag("/"), digit1);
        let (rest, subnet) = opt(subnet_parser)(input)?;

        Ok((rest, subnet))
    }

    /// <!--${internal}-->
    ///
    /// Parses `:1337`, extracting the `1337` part, and discarding the `:`.
    ///
    /// Returns `0` if it doesn't parse anything.
    pub(super) fn port(input: &str) -> IResult<&str, &str> {
        let port_parser = preceded(tag(":"), digit1);
        let (rest, port) = opt(port_parser)(input)?;

        let port = port.unwrap_or("0");

        Ok((rest, port))
    }
}

impl FromStr for OutgoingFilter {
    type Err = OutgoingFilterError;

    #[tracing::instrument(level = "trace", ret)]
    fn from_str(input: &str) -> Result<Self, Self::Err> {
        use crate::feature::network::outgoing::parser::*;

        // Perform the basic parsing.
        let (rest, protocol) = protocol(input)?;
        let (rest, address) = address(rest)?;
        let (rest, subnet) = subnet(rest)?;
        let (rest, port) = port(rest)?;

        // Stringify and convert to proper types.
        let protocol = protocol.parse()?;
        let port = port.parse::<u16>()?;

        let address = subnet
            .map(|subnet| format!("{address}/{subnet}").parse::<ipnet::IpNet>())
            .transpose()?
            .map_or_else(
                // Try to parse as an IPv4 address.
                || {
                    format!("{address}:{port}")
                        .parse::<SocketAddr>()
                        // Try again as IPv6.
                        .or_else(|_| format!("[{address}]:{port}").parse())
                        .map(AddressFilter::Socket)
                        // Neither IPv4 nor IPv6, it's probably a name.
                        .unwrap_or(AddressFilter::Name((address.to_string(), port)))
                },
                |subnet| AddressFilter::Subnet((subnet, port)),
            );

        if rest.is_empty() {
            Ok(Self { protocol, address })
        } else {
            Err(OutgoingFilterError::TrailingValue(rest.to_string()))
        }
    }
}

impl CollectAnalytics for &OutgoingConfig {
    fn collect_analytics(&self, analytics: &mut mirrord_analytics::Analytics) {
        analytics.add("tcp", self.tcp);
        analytics.add("udp", self.udp);
        analytics.add("ignore_localhost", self.ignore_localhost);
        analytics.add(
            "unix_streams",
            self.unix_streams
                .as_ref()
                .map(|v| v.len())
                .unwrap_or_default(),
        );

        if let Some(filter) = self.filter.as_ref() {
            match filter {
                OutgoingFilterConfig::Remote(value) => {
                    analytics.add("outgoing_filter_remote", value.len())
                }
                OutgoingFilterConfig::Local(value) => {
                    analytics.add("outgoing_filter_local", value.len())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use ipnet::IpNet;
    use rstest::{fixture, rstest};

    use super::*;
    use crate::{
        config::{ConfigContext, MirrordConfig},
        util::{testing::with_env_vars, ToggleableConfig},
    };

    #[rstest]
    fn default(
        #[values((None, true), (Some("false"), false), (Some("true"), true))] tcp: (
            Option<&str>,
            bool,
        ),
        #[values((None, true), (Some("false"), false), (Some("true"), true))] udp: (
            Option<&str>,
            bool,
        ),
    ) {
        with_env_vars(
            vec![
                ("MIRRORD_TCP_OUTGOING", tcp.0),
                ("MIRRORD_UDP_OUTGOING", udp.0),
            ],
            || {
                let mut cfg_context = ConfigContext::default();
                let outgoing = OutgoingFileConfig::default()
                    .generate_config(&mut cfg_context)
                    .unwrap();

                assert_eq!(outgoing.tcp, tcp.1);
                assert_eq!(outgoing.udp, udp.1);
            },
        );
    }

    #[rstest]
    fn disabled(
        #[values((None, false), (Some("false"), false), (Some("true"), true))] tcp: (
            Option<&str>,
            bool,
        ),
        #[values((None, false), (Some("false"), false), (Some("true"), true))] udp: (
            Option<&str>,
            bool,
        ),
    ) {
        with_env_vars(
            vec![
                ("MIRRORD_TCP_OUTGOING", tcp.0),
                ("MIRRORD_UDP_OUTGOING", udp.0),
            ],
            || {
                let mut cfg_context = ConfigContext::default();
                let outgoing = ToggleableConfig::<OutgoingFileConfig>::Enabled(false)
                    .generate_config(&mut cfg_context)
                    .unwrap();

                assert_eq!(outgoing.tcp, tcp.1);
                assert_eq!(outgoing.udp, udp.1);
            },
        );
    }

    // Valid configs.
    #[fixture]
    fn full() -> &'static str {
        "tcp://1.2.3.0/24:7777"
    }

    #[fixture]
    fn full_converted() -> OutgoingFilter {
        OutgoingFilter {
            protocol: ProtocolFilter::Tcp,
            address: AddressFilter::Subnet((IpNet::from_str("1.2.3.0/24").unwrap(), 7777)),
        }
    }

    #[fixture]
    fn ipv6() -> &'static str {
        "tcp://[2800:3f0:4001:81e::2004]:7777"
    }

    #[fixture]
    fn ipv6_converted() -> OutgoingFilter {
        OutgoingFilter {
            protocol: ProtocolFilter::Tcp,
            address: AddressFilter::Socket(
                SocketAddr::from_str("[2800:3f0:4001:81e::2004]:7777").unwrap(),
            ),
        }
    }

    #[fixture]
    fn protocol_only() -> &'static str {
        "tcp://"
    }

    #[fixture]
    fn protocol_only_converted() -> OutgoingFilter {
        OutgoingFilter {
            protocol: ProtocolFilter::Tcp,
            address: AddressFilter::Socket(SocketAddr::from_str("0.0.0.0:0").unwrap()),
        }
    }

    #[fixture]
    fn name() -> &'static str {
        "tcp://google.com:7777"
    }

    #[fixture]
    fn name_converted() -> OutgoingFilter {
        OutgoingFilter {
            protocol: ProtocolFilter::Tcp,
            address: AddressFilter::Name(("google.com".to_string(), 7777)),
        }
    }

    #[fixture]
    fn name_only() -> &'static str {
        "rust-lang.org"
    }

    #[fixture]
    fn name_only_converted() -> OutgoingFilter {
        OutgoingFilter {
            protocol: ProtocolFilter::Any,
            address: AddressFilter::Name(("rust-lang.org".to_string(), 0)),
        }
    }

    #[fixture]
    fn localhost() -> &'static str {
        "localhost"
    }

    #[fixture]
    fn localhost_converted() -> OutgoingFilter {
        OutgoingFilter {
            protocol: ProtocolFilter::Any,
            address: AddressFilter::Name(("localhost".to_string(), 0)),
        }
    }

    #[fixture]
    fn subnet_port() -> &'static str {
        "1.2.3.0/24:7777"
    }

    #[fixture]
    fn subnet_port_converted() -> OutgoingFilter {
        OutgoingFilter {
            protocol: ProtocolFilter::Any,
            address: AddressFilter::Subnet((IpNet::from_str("1.2.3.0/24").unwrap(), 7777)),
        }
    }

    #[fixture]
    fn subnet_only() -> &'static str {
        "1.2.3.0/24"
    }

    #[fixture]
    fn subnet_only_converted() -> OutgoingFilter {
        OutgoingFilter {
            protocol: ProtocolFilter::Any,
            address: AddressFilter::Subnet((IpNet::from_str("1.2.3.0/24").unwrap(), 0)),
        }
    }

    #[fixture]
    fn protocol_port() -> &'static str {
        "udp://:7777"
    }

    #[fixture]
    fn protocol_port_converted() -> OutgoingFilter {
        OutgoingFilter {
            protocol: ProtocolFilter::Udp,
            address: AddressFilter::Socket(SocketAddr::from_str("0.0.0.0:7777").unwrap()),
        }
    }

    #[fixture]
    fn port_only() -> &'static str {
        ":7777"
    }

    #[fixture]
    fn port_only_converted() -> OutgoingFilter {
        OutgoingFilter {
            protocol: ProtocolFilter::Any,
            address: AddressFilter::Socket(SocketAddr::from_str("0.0.0.0:7777").unwrap()),
        }
    }

    // Bad configs.
    #[fixture]
    fn name_with_subnet() -> &'static str {
        "tcp://google.com/24:7777"
    }

    #[fixture]
    fn port_protocol() -> &'static str {
        ":7777udp://"
    }

    #[fixture]
    fn fake_protocol() -> &'static str {
        "meow://"
    }

    #[rstest]
    #[case(full(), full_converted())]
    #[case(ipv6(), ipv6_converted())]
    #[case(protocol_only(), protocol_only_converted())]
    #[case(name(), name_converted())]
    #[case(name_only(), name_only_converted())]
    #[case(localhost(), localhost_converted())]
    #[case(subnet_port(), subnet_port_converted())]
    #[case(subnet_only(), subnet_only_converted())]
    #[case(protocol_port(), protocol_port_converted())]
    #[case(port_only(), port_only_converted())]
    fn valid_filters(#[case] input: &'static str, #[case] converted: OutgoingFilter) {
        assert_eq!(OutgoingFilter::from_str(input).unwrap(), converted);
    }

    #[rstest]
    #[case(name_with_subnet())]
    #[case(port_protocol())]
    #[case(fake_protocol())]
    #[should_panic]
    fn invalid_filters(#[case] input: &'static str) {
        OutgoingFilter::from_str(input).unwrap();
    }
}
