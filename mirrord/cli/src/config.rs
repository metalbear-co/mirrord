#![deny(missing_docs)]

use std::path::PathBuf;

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use mirrord_operator::setup::OperatorNamespace;

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
        path: String,
    },
    #[allow(dead_code)]
    #[command(skip)]
    Login(LoginArgs),
    /// Operator commands eg. setup
    #[command(hide = true)]
    Operator(Box<OperatorArgs>),

    /// List targets/resources like pods/namespaces in json format    
    #[command(hide = true, name = "ls")]
    ListTargets(Box<ListTargetArgs>),
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Debug)]
pub enum FsMode {
    /// Read & Write from remote, apart from overrides (hardcoded and configured in file)
    Write,
    /// Read from remote, Write local, apart from overrides (hardcoded and configured in file) -
    /// default
    Read,
    /// Read & Write from local (disabled)
    Local,
    /// Read & Write from local, apart from overrides (hardcoded and configured in file)
    LocalWithOverrides,
}

impl ToString for FsMode {
    fn to_string(&self) -> String {
        match self {
            FsMode::Local => "local".to_string(),
            FsMode::LocalWithOverrides => "localwithoverrides".to_string(),
            FsMode::Read => "read".to_string(),
            FsMode::Write => "write".to_string(),
        }
    }
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

    /// Default file system behavior: disabled, read, write, local
    #[arg(long)]
    pub fs_mode: Option<FsMode>,

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

    /// mirrord will not load into these processes, they will run completely locally.
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

    /// Pause target container while running.
    #[arg(short, long, alias = "paws")]
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

    /// Create a trace file of errors for debugging.
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

#[derive(Args, Debug)]
pub(super) struct OperatorArgs {
    #[command(subcommand)]
    pub command: OperatorCommand,
}

#[derive(Subcommand, Debug)]
pub(super) enum OperatorCommand {
    /// This will install the operator, which requires a seat based license to be used.
    ///
    /// NOTE: You don't need to install the operator to use open source mirrord features.
    #[command(override_usage = "mirrord operator setup [OPTIONS] | kubectl apply -f -")]
    Setup {
        /// ToS can be read here https://metalbear.co/legal/terms
        #[arg(long)]
        accept_tos: bool,

        /// License key to be stored in mirrord-operator-license secret
        #[arg(long)]
        license_key: Option<String>,

        /// Output to kubernetes specs to file instead of stdout and piping to kubectl
        #[arg(short, long)]
        file: Option<PathBuf>,

        /// Set namespace to setup operator in (this doesn't limit the namespaces the operator will
        /// be able to access)
        #[arg(short, long, default_value = "mirrord")]
        namespace: OperatorNamespace,
    },
}

#[derive(ValueEnum, Clone, Debug)]
pub enum Format {
    Json,
}

#[derive(Args, Debug)]
pub(super) struct ListTargetArgs {
    /// Specify the format of the output.
    #[arg(
        short = 'o',
        long = "output",
        value_name = "FORMAT",
        value_enum,
        default_value_t = Format::Json
    )]
    pub output: Format,

    /// Specify the namespace to list targets in.
    #[arg(short = 'n', long = "namespace")]
    pub namespace: Option<String>,
}
