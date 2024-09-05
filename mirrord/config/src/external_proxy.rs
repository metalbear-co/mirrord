use std::{net::IpAddr, path::PathBuf};

use mirrord_config_derive::MirrordConfig;
use schemars::JsonSchema;
use serde::Serialize;

use crate::config::source::MirrordConfigSource;

pub static MIRRORD_EXTERNAL_TLS_CERTIFICATE_ENV: &str = "MIRRORD_EXTERNAL_TLS_CERTIFICATE";
pub static MIRRORD_EXTERNAL_TLS_KEY_ENV: &str = "MIRRORD_EXTERNAL_TLS_KEY";

/// Configuration for the external proxy mirrord spawns when using the `mirrord container` command.
/// This proxy is used to allow the internal proxy running in sidecar to connect to the mirrord
/// agent.
///
/// If you get `ConnectionRefused` errors, increasing the timeouts a bit might solve the issue.
///
/// ```json
/// {
///   "external_proxy": {
///     "start_idle_timeout": 30,
///     "idle_timeout": 5
///   }
/// }
/// ```
#[derive(MirrordConfig, Clone, Debug, Serialize)]
#[config(map_to = "ExternalProxyFileConfig", derive = "JsonSchema")]
#[cfg_attr(test, config(derive = "PartialEq"))]
pub struct ExternalProxyConfig {
    /// ### external_proxy.listen {#external_proxy-listen}
    ///
    /// Provide a specific address to listen to for external proxy
    /// (will try and bind local machine ip if not specified)
    ///
    /// This is a workaround for when the local machine ip that is default is bound is not
    /// accecible from the container (local address resoved to `192.168.0.2` but said ip is
    /// inacceible from within a container)
    pub listen: Option<IpAddr>,

    /// ### external_proxy.address {#external_proxy-address}
    ///
    /// Specify an address that is accessible from within the container runtime to the host machine
    ///
    /// This is a workaround where the listen address should be different from the one the
    /// container is connecting to, example can be where `host.docker.internal -> 192.168.127.254`
    /// and we intend to utilize the network briging
    ///
    /// Note: this needs to be an ip because of TLS nagotiation.
    /// ```json
    /// {
    ///     "external_proxy": {
    ///         "listen": "127.0.0.1",
    ///         "address": "192.168.127.254"
    ///     }
    /// }
    /// ```
    pub address: Option<IpAddr>,

    /// <!--${internal}-->
    ///
    /// Certificate path to be used for wrapping external proxy tcp listener with a tcp acceptor
    /// (self-signed one will be generated automaticaly if not specified)
    #[config(env = MIRRORD_EXTERNAL_TLS_CERTIFICATE_ENV)]
    pub tls_certificate: Option<PathBuf>,

    /// <!--${internal}-->
    ///
    /// Private Key path to be used for wrapping external proxy tcp listener with a tcp acceptor
    /// (self-signed one will be generated automaticaly if not specified)
    #[config(env = MIRRORD_EXTERNAL_TLS_KEY_ENV)]
    pub tls_key: Option<PathBuf>,

    /// ### external_proxy.start_idle_timeout {#external_proxy-start_idle_timeout}
    ///
    /// How much time to wait for the first connection to the external proxy in seconds.
    ///
    /// Common cases would be running with dlv or any other debugger, which sets a breakpoint
    /// on process execution, delaying the layer startup and connection to the external proxy.
    ///
    /// ```json
    /// {
    ///   "external_proxy": {
    ///     "start_idle_timeout": 60
    ///   }
    /// }
    /// ```
    #[config(default = 60)]
    pub start_idle_timeout: u64,

    /// ### external_proxy.idle_timeout {#external_proxy-idle_timeout}
    ///
    /// How much time to wait while we don't have any active connections before exiting.
    ///
    /// Common cases would be running a chain of processes that skip using the layer
    /// and don't connect to the proxy.
    ///
    /// ```json
    /// {
    ///   "external_proxy": {
    ///     "idle_timeout": 30
    ///   }
    /// }
    /// ```
    #[config(default = 5)]
    pub idle_timeout: u64,

    /// ### external_proxy.log_level {#external_proxy-log_level}
    /// Sets the log level for the external proxy.
    ///
    /// Follows the `RUST_LOG` convention (i.e `mirrord=trace`), and will only be used if
    /// `external_proxy.log_destination` is set
    pub log_level: Option<String>,

    /// ### external_proxy.log_destination {#external_proxy-log_destination}
    /// Set the log file destination for the external proxy.
    pub log_destination: Option<String>,
}
