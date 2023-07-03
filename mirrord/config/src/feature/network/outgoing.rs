use core::str::FromStr;
use std::{collections::HashSet, net::SocketAddr};

use mirrord_analytics::CollectAnalytics;
use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use thiserror::Error;

use crate::{
    config::{from_env::FromEnv, source::MirrordConfigSource, ConfigError},
    util::{MirrordToggleableConfig, VecOrSingle},
};

/// Tunnel outgoing network operations through mirrord.
///
/// See the outgoing [reference](https://mirrord.dev/docs/reference/traffic/#outgoing) for more
/// details.
///
/// ```json
/// {
///   "feature": {
///     "network": {
///       "outgoing": {
///         "tcp": true,
///         "udp": true,
///         "ignore_localhost": false,
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

    pub remote: HashSet<String>,
    pub local: HashSet<String>,

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
    fn disabled_config() -> Result<Self::Generated, ConfigError> {
        Ok(OutgoingConfig {
            tcp: FromEnv::new("MIRRORD_TCP_OUTGOING")
                .source_value()
                .unwrap_or(Ok(false))?,
            udp: FromEnv::new("MIRRORD_UDP_OUTGOING")
                .source_value()
                .unwrap_or(Ok(false))?,
            unix_streams: FromEnv::new("MIRRORD_OUTGOING_REMOTE_UNIX_STREAMS")
                .source_value()
                .transpose()?,
            ..Default::default()
        })
    }
}

#[derive(Debug, Error)]
pub enum OutgoingFilterError {
    #[error("Nom: failed parsing with {0}!")]
    Nom2(nom::Err<nom::error::Error<Vec<u8>>>),

    #[error("IO: failed IO operation with {0}!")]
    IO(#[from] std::io::Error),

    #[error("Subnet: Failed parsing with {0}!")]
    Subnet(#[from] ipnet::AddrParseError),

    #[error("Utf8: Failed converting value from UTF8 with {0}!")]
    Utf8(#[from] std::str::Utf8Error),

    #[error("FromUtf8: Failed converting value from UTF8 with {0}!")]
    FromUtf8(#[from] std::string::FromUtf8Error),

    #[error("ParseInt: Failed converting string into `u16` with {0}!")]
    ParseInt(#[from] std::num::ParseIntError),

    #[error("Failed parsing protocol value of {0}!")]
    InvalidProtocol(String),

    #[error("Found trailing value after parsing {0}!")]
    TrailingValue(String),
}

impl From<nom::Err<nom::error::Error<&[u8]>>> for OutgoingFilterError {
    fn from(value: nom::Err<nom::error::Error<&[u8]>>) -> Self {
        Self::Nom2(value.to_owned())
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OutgoingAddress {
    Socket(SocketAddr),
    Name((String, u16)),
    Subnet((ipnet::IpNet, u16)),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OutgoingFilter {
    pub protocol: ProtocolFilter,
    pub address: OutgoingAddress,
}

/// It's dangerous to go alone!
/// Take [this](https://github.com/rust-bakery/nom/blob/main/doc/choosing_a_combinator.md).
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

    pub(super) fn protocol(input: &[u8]) -> IResult<&[u8], &[u8]> {
        let (input, protocol) = opt(terminated(take_until("://"), tag("://")))(input)?;
        let protocol = protocol.unwrap_or(b"any");

        Ok((input, protocol))
    }

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
    /// [`SocketAddr::parse`], or [`IpNet::parse`].
    pub(super) fn address(input: &[u8]) -> nom::IResult<&[u8], Vec<u8>> {
        let ipv6 = many1(alt((alphanumeric1, tag(b":"))));
        let ipv6_host = delimited(tag(b"["), ipv6, tag(b"]"));

        let name_or_ipv4 = alt((alphanumeric1, tag(b"-"), tag(b"_"), tag(b".")));
        let dotted_address = many1(name_or_ipv4);

        let (input, address) = opt(alt((dotted_address, ipv6_host)))(input)?;

        let address = address
            .map(|addr| addr.concat())
            .unwrap_or(b"0.0.0.0".to_vec());

        // TODO(alex) [mid] 2023-06-28: Convert this with `ToSocketAddrs` after we have parsed the
        // whole config, and thus have access to both addr and port.

        Ok((input, address))
    }

    pub(super) fn subnet(input: &[u8]) -> IResult<&[u8], Option<Vec<u8>>> {
        let subnet = preceded(tag(b"/"), many1(digit1));
        let (input, subnet) = opt(subnet)(input)?;

        let subnet = subnet.map(|s| s.concat());

        Ok((input, subnet))
    }

    pub(super) fn port(input: &[u8]) -> IResult<&[u8], Vec<u8>> {
        let port = preceded(tag(b":"), many1(digit1));
        let (input, port) = opt(port)(input)?;

        let port = port.map(|p| p.concat()).unwrap_or(b"0".to_vec());

        Ok((input, port))
    }
}

impl FromStr for OutgoingFilter {
    type Err = OutgoingFilterError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        use core::str;

        use crate::feature::network::outgoing::parser::*;

        println!("{input:?}");

        let input = input.as_bytes();

        // Perform the basic parsing.
        let (rest, protocol) = protocol(input)?;
        let (rest, address) = address(rest)?;
        let (rest, subnet) = subnet(rest)?;
        let (rest, port) = port(rest)?;

        // Stringify and convert to proper types.
        let protocol = str::from_utf8(protocol)?.parse()?;
        let address = str::from_utf8(&address)?;
        let subnet = subnet.map(|s| String::from_utf8(s)).transpose()?;
        let port = str::from_utf8(&port)?.parse::<u16>()?;

        println!("{address:#?} {port:#?}");

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
                        .map(OutgoingAddress::Socket)
                        // Neither IPv4 nor IPv6, it's probably a name.
                        .unwrap_or(OutgoingAddress::Name((address.to_string(), port)))
                },
                |subnet| OutgoingAddress::Subnet((subnet, port)),
            );

        if rest.is_empty() {
            Ok(Self { protocol, address })
        } else {
            Err(OutgoingFilterError::TrailingValue(
                str::from_utf8(rest)?.to_string(),
            ))
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
    }
}

#[cfg(test)]
mod tests {
    use ipnet::IpNet;
    use rstest::{fixture, rstest};

    use super::*;
    use crate::{
        config::MirrordConfig,
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
                let outgoing = OutgoingFileConfig::default().generate_config().unwrap();

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
                let outgoing = ToggleableConfig::<OutgoingFileConfig>::Enabled(false)
                    .generate_config()
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
            address: OutgoingAddress::Subnet((IpNet::from_str("1.2.3.0/24").unwrap(), 7777)),
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
            address: OutgoingAddress::Socket(
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
            address: OutgoingAddress::Socket(SocketAddr::from_str("0.0.0.0:0").unwrap()),
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
            address: OutgoingAddress::Name(("google.com".to_string(), 7777)),
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
            address: OutgoingAddress::Name(("rust-lang.org".to_string(), 0)),
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
            address: OutgoingAddress::Name(("localhost".to_string(), 0)),
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
            address: OutgoingAddress::Subnet((IpNet::from_str("1.2.3.0/24").unwrap(), 7777)),
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
            address: OutgoingAddress::Subnet((IpNet::from_str("1.2.3.0/24").unwrap(), 0)),
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
            address: OutgoingAddress::Socket(SocketAddr::from_str("0.0.0.0:7777").unwrap()),
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
            address: OutgoingAddress::Socket(SocketAddr::from_str("0.0.0.0:7777").unwrap()),
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
