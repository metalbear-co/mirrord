#![deny(missing_docs)]

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

        // TODO(alex): We should remove this arg from here and put into the general `Args`, but
        // this would be a breaking change, as the agent would be started as:
        // `agent --mesh targeted` becomes incompatible when a new layer version tries to
        // initialize an old agent version.
        /// Which kind of mesh the remote pod is in, see [`MeshVendor`].
        #[arg(long)]
        mesh: Option<MeshVendor>,
    },
    /// Inform the agent to use `proc/1/root` as the root directory.
    Ephemeral {
        // TODO(alex): Same issue here, we want to remove this at some point, and move it to
        // `Args`.
        /// Which kind of mesh the remote pod is in, see [`MeshVendor`].
        #[arg(long)]
        mesh: Option<MeshVendor>,
    },
    #[default]
    Targetless,
    Blackbox,
}

impl Mode {
    pub fn is_targetless(&self) -> bool {
        matches!(self, Mode::Targetless)
    }

    // TODO(alex): Remove this when `mesh` option is removed from `cli::Mode`, and put into
    // `cli::Args`.
    /// Digs into `Mode` subcomand to get the `MeshVendor`.
    pub(super) fn mesh(&self) -> Option<MeshVendor> {
        match *self {
            Mode::Targeted { mesh, .. } | Mode::Ephemeral { mesh, .. } => mesh,
            _ => None,
        }
    }
}

pub fn parse_args() -> Args {
    Args::try_parse().unwrap_or_else(|err| err.exit())
}
