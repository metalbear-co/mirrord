use clap::{Args, Parser, Subcommand};

#[derive(Parser)]
#[clap(author, version, about, long_about = None)]
pub(super) struct Cli {
    #[clap(subcommand)]
    pub(super) commands: Commands,
}

#[derive(Subcommand)]
pub(super) enum Commands {
    Exec(Box<ExecArgs>),
    Extract {
        #[clap(value_parser)]
        path: String,
    },
}

#[derive(Args, Debug)]
pub(super) struct ExecArgs {
    /// Pod name to mirror.
    #[clap(short, long, value_parser)]
    pub pod_name: String,

    /// Namespace of the pod to mirror. Defaults to "default".
    #[clap(short = 'n', long, value_parser)]
    pub pod_namespace: Option<String>,

    /// Namespace to place agent in.
    #[clap(short = 'a', long, value_parser)]
    pub agent_namespace: Option<String>,

    /// Agent log level
    #[clap(short = 'l', long, value_parser)]
    pub agent_log_level: Option<String>,

    /// Agent image
    #[clap(short = 'i', long, value_parser)]
    pub agent_image: Option<String>,

    /// Enable file hooking
    #[clap(short = 'f', long, value_parser)]
    pub enable_fs: bool,

    /// The env vars to filter out
    #[clap(short = 'x', long, value_parser)]
    pub override_env_vars_exclude: Option<String>,

    /// The env vars to select
    #[clap(short = 's', long, value_parser)]
    pub override_env_vars_include: Option<String>,

    /// Binary to execute and mirror traffic into.
    #[clap(value_parser)]
    pub binary: String,

    /// Agent TTL
    #[clap(long, value_parser)]
    pub agent_ttl: Option<u16>,

    /// Select container name to impersonate. Default is first container.
    #[clap(long, value_parser)]
    pub impersonate_container_name: Option<String>,

    /// Accept/reject invalid certificates.
    #[clap(short = 'c', long, value_parser)]
    pub accept_invalid_certificates: bool,

    /// Arguments to pass to the binary.
    #[clap(value_parser)]
    pub(super) binary_args: Vec<String>,

    /// Where to extract the library to (defaults to a temp dir)
    #[clap(long, value_parser)]
    pub extract_path: Option<String>,
}
