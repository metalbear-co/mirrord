use std::{
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    num::ParseIntError,
    str::FromStr,
};

use nom::{
    branch::alt,
    bytes::complete::{tag, take_until},
    character::complete::{alphanumeric1, digit1},
    combinator::opt,
    multi::many1,
    sequence::{delimited, preceded, terminated},
    IResult,
};
use thiserror::Error;

/// The protocols we support in [`ProtocolAndAddressFilter`].
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ProtocolFilter {
    #[default]
    Any,
    Tcp,
    Udp,
}

#[derive(Error, Debug)]
#[error("invalid protocol: {0}")]
pub struct ParseProtocolError(String);

impl FromStr for ProtocolFilter {
    type Err = ParseProtocolError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let lowercase = s.to_lowercase();

        match lowercase.as_str() {
            "any" => Ok(Self::Any),
            "tcp" => Ok(Self::Tcp),
            "udp" => Ok(Self::Udp),
            invalid => Err(ParseProtocolError(invalid.to_string())),
        }
    }
}

/// <!--${internal}-->
/// Parsed addresses can be one of these 3 variants.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AddressFilter {
    /// Only port was specified.
    Port(u16),

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
    Name(String, u16),

    /// Just a plain old subnet and a port, specified as `a.b.c.d/e:f`.
    Subnet(ipnet::IpNet, u16),
}

impl AddressFilter {
    pub fn port(&self) -> u16 {
        match self {
            Self::Port(port) => *port,
            Self::Name(_, port) => *port,
            Self::Socket(socket) => socket.port(),
            Self::Subnet(_, port) => *port,
        }
    }
}

#[derive(Error, Debug)]
pub enum AddressFilterError {
    #[error("parsing with nom failed: {0}")]
    Nom(nom::Err<nom::error::Error<String>>),

    #[error("parsing port number failed: {0}")]
    ParsePort(ParseIntError),

    #[error("parsing left trailing value: {0}")]
    TrailingValue(String),

    #[error("parsing subnet prefix length failed: {0}")]
    ParseSubnetPrefixLength(ParseIntError),

    #[error("parsing subnet base IP address failed")]
    ParseSubnetBaseAddress,

    #[error("invalid subnet: {0}")]
    SubnetPrefixLen(#[from] ipnet::PrefixLenError),

    #[error("provided empty string")]
    Empty,
}

impl From<nom::Err<nom::error::Error<&str>>> for AddressFilterError {
    fn from(value: nom::Err<nom::error::Error<&str>>) -> Self {
        Self::Nom(value.to_owned())
    }
}

impl FromStr for AddressFilter {
    type Err = AddressFilterError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        // Perform the basic parsing.
        let (rest, address) = address(input)?;
        let (rest, subnet) = subnet(rest)?;
        let (rest, port) = port(rest)?;

        if !rest.is_empty() {
            return Err(Self::Err::TrailingValue(rest.to_string()));
        }

        match (address, subnet, port) {
            // Only port specified.
            (None, None, Some(port)) => {
                let port = port.parse::<u16>().map_err(AddressFilterError::ParsePort)?;

                Ok(Self::Port(port))
            }

            // Subnet specified. Address must be IP.
            (Some(address), Some(subnet), port) => {
                let as_ip = address
                    .parse::<Ipv4Addr>()
                    .map(IpAddr::from)
                    .or_else(|_| format!("[{address}]").parse::<Ipv6Addr>().map(IpAddr::from))
                    .map_err(|_| AddressFilterError::ParseSubnetBaseAddress)?;
                let prefix_len = subnet
                    .parse::<u8>()
                    .map_err(AddressFilterError::ParseSubnetPrefixLength)?;
                let ip_net = ipnet::IpNet::new(as_ip, prefix_len)?;

                let port = port
                    .map(u16::from_str)
                    .transpose()
                    .map_err(AddressFilterError::ParsePort)?
                    .unwrap_or(0);

                Ok(Self::Subnet(ip_net, port))
            }

            // Subnet not specified. Address can be a name or an IP.
            (Some(address), None, _) => {
                let port = port
                    .map(u16::from_str)
                    .transpose()
                    .map_err(AddressFilterError::ParsePort)?
                    .unwrap_or(0);

                let result = address
                    .parse::<Ipv4Addr>()
                    .map(IpAddr::from)
                    .or_else(|_| format!("[{address}]").parse::<Ipv6Addr>().map(IpAddr::from))
                    .map(|ip| Self::Socket(SocketAddr::new(ip, port)))
                    .unwrap_or(Self::Name(address, port));

                Ok(result)
            }

            // Subnet specified but address is missing, error.
            (None, Some(_), _) => Err(AddressFilterError::ParseSubnetBaseAddress),

            // Nothing is specified, error.
            (None, None, None) => Err(AddressFilterError::Empty),
        }
    }
}

/// <!--${internal}-->
/// The parsed filter with its [`ProtocolFilter`] and [`AddressFilter`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ProtocolAndAddressFilter {
    /// Valid protocol types.
    pub protocol: ProtocolFilter,

    /// Address|name|subnet we're going to filter.
    pub address: AddressFilter,
}

#[derive(Error, Debug)]
pub enum ProtocolAndAddressFilterError {
    #[error(transparent)]
    Address(#[from] AddressFilterError),
    #[error(transparent)]
    Protocol(#[from] ParseProtocolError),
}

impl From<nom::Err<nom::error::Error<&str>>> for ProtocolAndAddressFilterError {
    fn from(value: nom::Err<nom::error::Error<&str>>) -> Self {
        Self::Address(value.into())
    }
}

impl FromStr for ProtocolAndAddressFilter {
    type Err = ProtocolAndAddressFilterError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        // Perform the basic parsing.
        let (rest, protocol) = protocol(input)?;
        let protocol = protocol.parse()?;

        let address = rest.parse()?;

        Ok(Self { protocol, address })
    }
}

/// <!--${internal}-->
///
/// Parses `tcp://`, extracting the `tcp` part, and discarding the `://`.
fn protocol(input: &str) -> IResult<&str, &str> {
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
fn address(input: &str) -> IResult<&str, Option<String>> {
    let ipv6 = many1(alt((alphanumeric1, tag(":"))));
    let ipv6_host = delimited(tag("["), ipv6, tag("]"));

    let host_char = alt((alphanumeric1, tag("-"), tag("_"), tag(".")));
    let dotted_address = many1(host_char);

    let (rest, address) = opt(alt((dotted_address, ipv6_host)))(input)?;

    let address = address.map(|addr| addr.concat());

    Ok((rest, address))
}

/// <!--${internal}-->
///
/// Parses `/24`, extracting the `24` part, and discarding the `/`.
fn subnet(input: &str) -> IResult<&str, Option<&str>> {
    let subnet_parser = preceded(tag("/"), digit1);
    let (rest, subnet) = opt(subnet_parser)(input)?;

    Ok((rest, subnet))
}

/// <!--${internal}-->
///
/// Parses `:1337`, extracting the `1337` part, and discarding the `:`.
///
/// Returns [`None`] if it doesn't parse anything.
fn port(input: &str) -> IResult<&str, Option<&str>> {
    let port_parser = preceded(tag(":"), digit1);
    let (rest, port) = opt(port_parser)(input)?;

    Ok((rest, port))
}

#[cfg(test)]
mod tests {
    use ipnet::IpNet;
    use rstest::{fixture, rstest};

    use super::*;

    // Valid configs.
    #[fixture]
    fn full() -> &'static str {
        "tcp://1.2.3.0/24:7777"
    }

    #[fixture]
    fn full_converted() -> ProtocolAndAddressFilter {
        ProtocolAndAddressFilter {
            protocol: ProtocolFilter::Tcp,
            address: AddressFilter::Subnet(IpNet::from_str("1.2.3.0/24").unwrap(), 7777),
        }
    }

    #[fixture]
    fn ipv6() -> &'static str {
        "tcp://[2800:3f0:4001:81e::2004]:7777"
    }

    #[fixture]
    fn ipv6_converted() -> ProtocolAndAddressFilter {
        ProtocolAndAddressFilter {
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
    fn protocol_only_converted() -> ProtocolAndAddressFilter {
        ProtocolAndAddressFilter {
            protocol: ProtocolFilter::Tcp,
            address: AddressFilter::Socket(SocketAddr::from_str("0.0.0.0:0").unwrap()),
        }
    }

    #[fixture]
    fn name() -> &'static str {
        "tcp://google.com:7777"
    }

    #[fixture]
    fn name_converted() -> ProtocolAndAddressFilter {
        ProtocolAndAddressFilter {
            protocol: ProtocolFilter::Tcp,
            address: AddressFilter::Name("google.com".to_string(), 7777),
        }
    }

    #[fixture]
    fn name_only() -> &'static str {
        "rust-lang.org"
    }

    #[fixture]
    fn name_only_converted() -> ProtocolAndAddressFilter {
        ProtocolAndAddressFilter {
            protocol: ProtocolFilter::Any,
            address: AddressFilter::Name("rust-lang.org".to_string(), 0),
        }
    }

    #[fixture]
    fn localhost() -> &'static str {
        "localhost"
    }

    #[fixture]
    fn localhost_converted() -> ProtocolAndAddressFilter {
        ProtocolAndAddressFilter {
            protocol: ProtocolFilter::Any,
            address: AddressFilter::Name("localhost".to_string(), 0),
        }
    }

    #[fixture]
    fn subnet_port() -> &'static str {
        "1.2.3.0/24:7777"
    }

    #[fixture]
    fn subnet_port_converted() -> ProtocolAndAddressFilter {
        ProtocolAndAddressFilter {
            protocol: ProtocolFilter::Any,
            address: AddressFilter::Subnet(IpNet::from_str("1.2.3.0/24").unwrap(), 7777),
        }
    }

    #[fixture]
    fn subnet_only() -> &'static str {
        "1.2.3.0/24"
    }

    #[fixture]
    fn subnet_only_converted() -> ProtocolAndAddressFilter {
        ProtocolAndAddressFilter {
            protocol: ProtocolFilter::Any,
            address: AddressFilter::Subnet(IpNet::from_str("1.2.3.0/24").unwrap(), 0),
        }
    }

    #[fixture]
    fn protocol_port() -> &'static str {
        "udp://:7777"
    }

    #[fixture]
    fn protocol_port_converted() -> ProtocolAndAddressFilter {
        ProtocolAndAddressFilter {
            protocol: ProtocolFilter::Udp,
            address: AddressFilter::Socket(SocketAddr::from_str("0.0.0.0:7777").unwrap()),
        }
    }

    #[fixture]
    fn port_only() -> &'static str {
        ":7777"
    }

    #[fixture]
    fn port_only_converted() -> ProtocolAndAddressFilter {
        ProtocolAndAddressFilter {
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
    fn valid_filters(#[case] input: &'static str, #[case] converted: ProtocolAndAddressFilter) {
        assert_eq!(
            ProtocolAndAddressFilter::from_str(input).unwrap(),
            converted
        );
    }

    #[rstest]
    #[case(name_with_subnet())]
    #[case(port_protocol())]
    #[case(fake_protocol())]
    #[should_panic]
    fn invalid_filters(#[case] input: &'static str) {
        ProtocolAndAddressFilter::from_str(input).unwrap();
    }
}
