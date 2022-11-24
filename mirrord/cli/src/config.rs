use std::path::PathBuf;

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};

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
    // Login(LoginArgs),
}

#[derive(Args, Debug)]
#[clap(group(
    ArgGroup::new("exec")
        .required(true)
        .multiple(true)
        .args(&["target", "config-file"]),
))]
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum)]
enum FsMode {
    ReadWrite,
    Read,
    Local,
    Disabled,
}

pub(super) struct ExecArgs {
    /// Target name to mirror.    
    /// Target can either be a deployment or a pod.
    /// Valid formats: deployment/name, pod/name, pod/name/container/name
    #[clap(short = 't', long, value_parser)]
    pub target: Option<String>,

    /// Namespace of the pod to mirror. Defaults to "default".
    #[clap(short = 'n', long, value_parser)]
    pub target_namespace: Option<String>,

    /// Namespace to place agent in.
    #[clap(short = 'a', long, value_parser)]
    pub agent_namespace: Option<String>,

    /// Agent log level
    #[clap(short = 'l', long, value_parser)]
    pub agent_log_level: Option<String>,

    /// Agent image
    #[clap(short = 'i', long, value_parser)]
    pub agent_image: Option<String>,

    /// Disable file read only
    #[clap(long, value_parser)]
    pub no_fs: bool,

    /// Enable file hooking (Both R/W)
    #[clap(long = "rw", value_parser)]
    pub enable_rw_fs: bool,

    /// Default file system behavior: disabled, read, write, local
    #[clap(value_parser)]
    pub fs_mode: Option<FsMode>,

    /// The env vars to filter out
    #[clap(short = 'x', long, value_parser)]
    pub override_env_vars_exclude: Option<String>,

    /// The env vars to select. Default is '*'
    #[clap(short = 's', long, value_parser)]
    pub override_env_vars_include: Option<String>,

    /// Disables resolving a remote DNS.
    #[clap(long, value_parser)]
    pub no_remote_dns: bool,

    /// Binary to execute and connect with the remote pod.
    #[clap(value_parser)]
    pub binary: String,

    /// Binary to execute and connect with the remote pod.
    #[clap(long, value_parser)]
    pub skip_processes: Option<String>,

    /// Agent TTL
    #[clap(long, value_parser)]
    pub agent_ttl: Option<u16>,

    /// Agent Startup Timeout seconds
    #[clap(long, value_parser)]
    pub agent_statup_timeout: Option<u16>,

    /// Accept/reject invalid certificates.
    #[clap(short = 'c', long, value_parser)]
    pub accept_invalid_certificates: bool,

    /// Arguments to pass to the binary.
    #[clap(value_parser)]
    pub(super) binary_args: Vec<String>,

    /// Where to extract the library to. Default is temp dir.
    #[clap(long, value_parser)]
    pub extract_path: Option<String>,

    /// Use an Ephemeral Container to mirror traffic.
    #[clap(short, long, value_parser)]
    pub ephemeral_container: bool,

    /// Steal TCP instead of mirroring
    #[clap(long = "steal", value_parser)]
    pub tcp_steal: bool,

    /// Disable tcp/udp outgoing traffic
    #[clap(long, value_parser)]
    pub no_outgoing: bool,

    /// Disable tcp outgoing feature.
    #[clap(long, value_parser)]
    pub no_tcp_outgoing: bool,

    /// Disable udp outgoing feature.
    #[clap(long, value_parser)]
    pub no_udp_outgoing: bool,

    /// Disable telemetry - this also disables version check. See https://github.com/metalbear-co/mirrord/blob/main/TELEMETRY.md
    #[clap(long, value_parser)]
    pub no_telemetry: bool,

    /// Load config from config file
    #[clap(short = 'f', long, value_parser)]
    pub config_file: Option<PathBuf>,

    #[clap(long, value_parser)]
    pub capture_error_trace: bool,
}

#[derive(Args, Debug)]
pub(super) struct LoginArgs {
    /// Manualy insert token
    #[clap(long)]
    pub token: Option<String>,

    /// Time to wait till close the connection wating for reply from identity server
    #[clap(long, default_value = "120")]
    pub timeout: u64,

    /// Override identity server url
    #[clap(long, default_value = "https://identity.metalbear.dev")]
    pub auth_server: String,

    /// Don't open web browser automatically and just print url
    #[clap(long)]
    pub no_open: bool,
}
