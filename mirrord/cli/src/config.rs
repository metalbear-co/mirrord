#![deny(missing_docs)]

use std::path::PathBuf;

use clap::{ArgGroup, Args, Parser, Subcommand};

#[derive(Parser)]
#[command(author, version, about, long_about = None)]
pub(super) struct Cli {
    #[command(subcommand)]
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
#[command(group(
    ArgGroup::new("exec")
        .required(true)
        .multiple(true)
        .args(&["target", "config_file"]),
))]
pub(super) struct ExecArgs {
    /// Target name to mirror.    
    /// Target can either be a deployment or a pod.
    /// Valid formats: deployment/name, pod/name, pod/name/container/name
    #[arg(short = 't', long)]
    pub target: Option<String>,

    /// Namespace of the pod to mirror. Defaults to "default".
    #[arg(short = 'n', long)]
    pub target_namespace: Option<String>,

    /// Namespace to place agent in.
    #[arg(short = 'a', long)]
    pub agent_namespace: Option<String>,

    /// Agent log level
    #[arg(short = 'l', long)]
    pub agent_log_level: Option<String>,

    /// Agent image
    #[arg(short = 'i', long)]
    pub agent_image: Option<String>,

    /// Disable file read only
    #[arg(long)]
    pub no_fs: bool,

    /// Enable file hooking (Both R/W)
    #[arg(long = "rw")]
    pub enable_rw_fs: bool,

    /// The env vars to filter out
    #[arg(short = 'x', long)]
    pub override_env_vars_exclude: Option<String>,

    /// The env vars to select. Default is '*'
    #[arg(short = 's', long)]
    pub override_env_vars_include: Option<String>,

    /// Disables resolving a remote DNS.
    #[arg(long)]
    pub no_remote_dns: bool,

    /// Binary to execute and connect with the remote pod.
    pub binary: String,

    /// Binary to execute and connect with the remote pod.
    #[arg(long)]
    pub skip_processes: Option<String>,

    /// Agent TTL
    #[arg(long)]
    pub agent_ttl: Option<u16>,

    /// Agent Startup Timeout seconds
    #[arg(long)]
    pub agent_startup_timeout: Option<u16>,

    /// Accept/reject invalid certificates.
    #[arg(short = 'c', long)]
    pub accept_invalid_certificates: bool,

    /// Arguments to pass to the binary.
    pub(super) binary_args: Vec<String>,

    /// Where to extract the library to. Default is temp dir.
    #[arg(long)]
    pub extract_path: Option<String>,

    /// Use an Ephemeral Container to mirror traffic.
    #[arg(short, long)]
    pub ephemeral_container: bool,

    /// Steal TCP instead of mirroring
    #[arg(long = "steal")]
    pub tcp_steal: bool,

    /// Pause target pod(?TODO) while running.
    #[arg(short, long)]
    pub pause: bool,

    /// Disable tcp/udp outgoing traffic
    #[arg(long)]
    pub no_outgoing: bool,

    /// Disable tcp outgoing feature.
    #[arg(long)]
    pub no_tcp_outgoing: bool,

    /// Disable udp outgoing feature.
    #[arg(long)]
    pub no_udp_outgoing: bool,

    /// Disable telemetry - this also disables version check. See https://github.com/metalbear-co/mirrord/blob/main/TELEMETRY.md
    #[arg(long)]
    pub no_telemetry: bool,

    /// Load config from config file
    #[arg(short = 'f', long)]
    pub config_file: Option<PathBuf>,

    // Create a trace file of errors for debugging.
    #[arg(long)]
    pub capture_error_trace: bool,
}

#[derive(Args, Debug)]
pub(super) struct LoginArgs {
    /// Manualy insert token
    #[arg(long)]
    pub token: Option<String>,

    /// Time to wait till close the connection wating for reply from identity server
    #[arg(long, default_value_t = 120)]
    pub timeout: u64,

    /// Override identity server url
    #[arg(long, default_value = "https://identity.metalbear.dev")]
    pub auth_server: String,

    /// Don't open web browser automatically and just print url
    #[arg(long)]
    pub no_open: bool,
}
