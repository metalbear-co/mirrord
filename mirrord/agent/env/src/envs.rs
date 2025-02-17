//! Definitions of environment variables used to configure the mirrord-agent.
//!
//! If you want to add some more, please do it here.

use std::net::{IpAddr, SocketAddr};

use crate::{checked_env::CheckedEnv, steal_tls::StealPortTlsConfig};

/// Used to pass operator's x509 certificate to the agent.
///
/// This way the agent can be sure that it only accepts TLS connections coming from the exact
/// operator that spawned it.
pub const OPERATOR_CERT: CheckedEnv<String> = CheckedEnv::new("AGENT_OPERATOR_CERT_ENV");

/// Determines a network interface for mirroring.
pub const NETWORK_INTERFACE: CheckedEnv<String> = CheckedEnv::new("AGENT_NETWORK_INTERFACE_ENV");

/// Enables Prometheus metrics export point and sets its address.
pub const METRICS: CheckedEnv<SocketAddr> = CheckedEnv::new("MIRRORD_AGENT_METRICS");

/// Used to inform the agent that the target pod is in a mesh.
pub const IN_SERVICE_MESH: CheckedEnv<bool> = CheckedEnv::new("MIRRORD_AGENT_IN_SERVICE_MESH");

/// Used to inform the agent that the target pod is in an Istio CNI mesh.
pub const ISTIO_CNI: CheckedEnv<bool> = CheckedEnv::new("MIRRORD_AGENT_ISTIO_CNI");

/// Instructs the agent to flush connections when adding new iptables rules.
pub const STEALER_FLUSH_CONNECTIONS: CheckedEnv<bool> =
    CheckedEnv::new("MIRRORD_AGENT_STEALER_FLUSH_CONNECTIONS");

/// Instructs the agent to use `iptables-nft` instead of `iptables-legacy` for manipulating
/// iptables.
pub const NFTABLES: CheckedEnv<bool> = CheckedEnv::new("MIRRORD_AGENT_NFTABLES");

/// Instructs the agent to produce logs in JSON format.
pub const JSON_LOG: CheckedEnv<bool> = CheckedEnv::new("MIRRORD_AGENT_JSON_LOG");

/// Enables IPv6 support in the agent.
pub const IPV6_SUPPORT: CheckedEnv<bool> = CheckedEnv::new("AGENT_IPV6_ENV");

/// Sets a hard timeout on DNS queries.
pub const DNS_TIMEOUT: CheckedEnv<u32> = CheckedEnv::new("MIRRORD_AGENT_DNS_TIMEOUT");

/// Sets a hard limit on DNS query attempts.
pub const DNS_ATTEMPTS: CheckedEnv<u32> = CheckedEnv::new("MIRRORD_AGENT_DNS_ATTEMPTS");

/// Used in incoming traffic redirection to produce correct iptables rules.
pub const POD_IPS: CheckedEnv<Vec<IpAddr>> = CheckedEnv::new("MIRRORD_AGENT_POD_IPS");

/// Sets agent log level.
///
/// Should follow `tracing` format, e.g `mirrord=trace`.
pub const LOG_LEVEL: CheckedEnv<String> = CheckedEnv::new("RUST_LOG");

/// Provides the agent with a steal TLS configuration.
///
/// Not yet used by the agent.
pub const STEAL_TLS_CONFIG: CheckedEnv<Vec<StealPortTlsConfig>> =
    CheckedEnv::new("MIRRORD_AGENT_STEAL_TLS_CONFIG");
