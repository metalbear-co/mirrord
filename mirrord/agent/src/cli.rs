#![deny(missing_docs)]

use std::net::SocketAddr;

use clap::{Parser, Subcommand};
use mirrord_agent_env::envs;

const DEFAULT_RUNTIME: &str = "containerd";

/// **Heads-up**: Order of arguments passed to this matter, so if you add a new arg after something
/// like `Targeted`, it won't work, as `Mode` is a `subcommand`. You would need to add the arg in
/// `Mode::Targeted` for it to work (that's why `--mesh` is there and not here).
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    #[clap(subcommand)]
    pub mode: Mode,

    /// Port to use for communication
    #[arg(short = 'l', long, default_value_t = 61337)]
    pub communicate_port: u16,

    /// Communication timeout in seconds
    #[arg(short = 't', long, default_value_t = 30)]
    pub communication_timeout: u16,

    /// Interface to use
    #[arg(short = 'i', long, env = envs::NETWORK_INTERFACE.name)]
    pub network_interface: Option<String>,

    /// Controls whether metrics are enabled, and the address to set up the metrics server.
    #[arg(long, env = envs::METRICS.name)]
    pub metrics: Option<SocketAddr>,

    /// Return an error after accepting the first client connection, in order to test agent error
    /// cleanup.
    ///
    /// ## Internal
    ///
    /// This feature is used internally for debugging purposes only!
    #[arg(long, default_value_t = false, hide = true)]
    pub test_error: bool,

    /// PEM-encoded X509 certificate that this agent will use to secure incoming TCP connections
    /// from the clients (proxied by the operator).
    ///
    /// If not given, the agent will not use TLS.
    #[arg(long, env = envs::OPERATOR_CERT.name)]
    pub operator_tls_cert_pem: Option<String>,

    /// Whether there is a mesh present in the target pod.
    #[arg(
        long,
        default_value_t = false,
        hide = true,
        env = envs::IN_SERVICE_MESH.name,
    )]
    pub is_mesh: bool,

    /// Enable support for IPv6-only clusters
    ///
    /// Only when this option is set will take the needed steps to run on an IPv6 single stack
    /// cluster.
    #[arg(long, default_value_t = false, env = envs::IPV6_SUPPORT.name)]
    pub ipv6: bool,
}

impl Args {
    pub fn is_mesh(&self) -> bool {
        self.is_mesh
            || matches!(
                self.mode,
                Mode::Targeted { mesh: Some(_), .. } | Mode::Ephemeral { mesh: Some(_), .. }
            )
    }
}

#[derive(Clone, Debug, Default, Subcommand)]
pub enum Mode {
    Targeted {
        /// Container id to get traffic from
        #[arg(short, long)]
        container_id: String,

        /// Container runtime to use
        #[arg(short = 'r', long, default_value = DEFAULT_RUNTIME)]
        container_runtime: String,

        // This argument is being kept here only for compatibility with very old CLIs.
        #[arg(long)]
        mesh: Option<String>,
    },
    /// Inform the agent to use `proc/1/root` as the root directory.
    Ephemeral {
        // This argument is being kept here only for compatibility with very old CLIs.
        #[arg(long)]
        mesh: Option<String>,

        /// Container id to get traffic from
        #[arg(short, long)]
        container_id: Option<String>,
    },
    #[default]
    Targetless,
    #[clap(hide = true)]
    BlackboxTest,
}

impl Mode {
    pub fn is_targetless(&self) -> bool {
        matches!(self, Mode::Targetless)
    }
}

pub fn parse_args() -> Args {
    Args::try_parse().unwrap_or_else(|err| err.exit())
}
