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

    /// Which kind of mesh the remote pod is in, see [`MeshVendor`].
    #[arg(long)]
    pub(super) mesh: Option<MeshVendor>,
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

        // TODO(alex): Remove this `mesh` arg from here at some point. It'll be a breaking change
        // until all users have an agent image that can deal with `--mesh` from `Args`.
        /// Which kind of mesh the remote pod is in, see [`MeshVendor`].
        #[deprecated = "Moved to be part of general `Args`, remains here to avoid breaking change!"]
        #[arg(long)]
        mesh: Option<MeshVendor>,
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

    // TODO(alex): Remove this when `mesh` option is removed from `cli::Mode`.
    /// Digs into `Mode::Targeted` to get the `MeshVendor`.
    #[allow(deprecated)]
    #[deprecated = "`mesh` was moved to `Args`."]
    pub(super) fn mesh(&self) -> Option<MeshVendor> {
        match *self {
            Mode::Targeted { mesh, .. } => mesh,
            _ => None,
        }
    }
}

pub fn parse_args() -> Args {
    Args::try_parse().unwrap_or_else(|err| err.exit())
}
