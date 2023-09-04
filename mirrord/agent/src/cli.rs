#![deny(missing_docs)]

use clap::{Parser, Subcommand};

const DEFAULT_RUNTIME: &str = "containerd";

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
    #[arg(short = 'i', long)]
    pub network_interface: Option<String>,

    /// Pause the target container while clients are connected.
    #[arg(short = 'p', long, default_value_t = false)]
    pub pause: bool,

    /// Return an error after accepting the first client connection, in order to test agent error
    /// cleanup.
    ///
    /// ## Internal
    ///
    /// This feature is used internally for debugging purposes only!
    #[arg(long, default_value_t = false, hide = true)]
    pub test_error: bool,

    #[arg(long, default_value = "1.2.1")]
    pub base_protocol_version: semver::Version,
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
    },
    /// Inform the agent to use `proc/1/root` as the root directory.
    Ephemeral,
    #[cfg(test)]
    Test,
    #[default]
    Targetless,
}

pub fn parse_args() -> Args {
    Args::try_parse().unwrap_or_else(|err| err.exit())
}
