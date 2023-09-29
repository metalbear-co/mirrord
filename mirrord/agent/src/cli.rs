#![deny(missing_docs)]

use std::str::FromStr;

use clap::{Parser, Subcommand};
use mirrord_protocol::MeshVendor;

const DEFAULT_RUNTIME: &str = "containerd";

/// **Heads-up**: Order of arguments passed to this matter, so if you add a new arg after something
/// like `Targeted`, it won't work, as `Mode` is a `subcomand`. You would need to add the arg in
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

        /// Which kind of mesh the remote pod is in, see [`MeshVendor`].
        #[arg(long)]
        mesh: Option<String>,
    },
    /// Inform the agent to use `proc/1/root` as the root directory.
    Ephemeral,
    #[default]
    Targetless,
    #[clap(hide = true)]
    BlackboxTest,
}

impl Mode {
    pub fn is_targetless(&self) -> bool {
        matches!(self, Mode::Targetless)
    }

    /// Converts a mesh `Option<String>` to an `Option<MeshVendor>`.
    ///
    /// Keep in mind that the [`FromStr`] implementation for [`MeshVendor`] `panic`s if we feed it
    /// anything other than `"Istio"` or `"Linkerd"`.
    pub(super) fn mesh(&self) -> Option<MeshVendor> {
        match self {
            Mode::Targeted { mesh, .. } => mesh
                .as_ref()
                .and_then(|mesh| MeshVendor::from_str(mesh).ok()),
            _ => None,
        }
    }
}

pub fn parse_args() -> Args {
    Args::try_parse().unwrap_or_else(|err| err.exit())
}
