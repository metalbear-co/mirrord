#![deny(missing_docs)]

use std::path::PathBuf;

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum};
use email_address::EmailAddress;
use mirrord_operator::setup::OperatorNamespace;

#[derive(Parser)]
#[command(
    author,
    version,
    about,
    long_about = r#"
Encountered an issue? Have a feature request?
Join our Discord server at https://discord.gg/metalbear, create a GitHub issue at https://github.com/metalbear-co/mirrord/issues/new/choose, or email as at hi@metalbear.co"#
)]
pub(super) struct Cli {
    #[command(subcommand)]
    pub(super) commands: Commands,
}

#[derive(Subcommand)]
pub(super) enum Commands {
    /// Execute a binary using mirrord, mirror remote traffic to it, provide it access to remote
    /// resources (network, files) and environment variables.
    Exec(Box<ExecArgs>),

    /// Register an email address to the waitlist for mirrord for Teams (`mirrord waitlist
    /// myemail@gmail.com`)
    ///
    /// mirrord for Teams is currently invite-only, and features include:
    ///
    /// 1. Traffic stealing/mirroring from multi-pod deployments.
    ///
    /// 2. Concurrent mirrord sessions on the same resource (e.g. multiple users using the same
    /// pod/deployment).
    ///
    /// 3. No privileged permissions required for end users.
    Waitlist(WaitlistArgs),

    #[command(hide = true)]
    Extract { path: String },
    /// Operator commands eg. setup
    Operator(Box<OperatorArgs>),

    /// List targets/resources like pods/namespaces in json format
    #[command(hide = true, name = "ls")]
    ListTargets(Box<ListTargetArgs>),

    /// Extension execution - used by extension to execute binaries.
    #[command(hide = true, name = "ext")]
    ExtensionExec(Box<ExtensionExecArgs>),

    /// Internal proxy - used to aggregate connections from multiple layers
    #[command(hide = true, name = "intproxy")]
    InternalProxy(Box<InternalProxyArgs>),
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
#[command(group(ArgGroup::new("exec")))]
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

    /// Default file system behavior: read, write, local
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

    /// Disable telemetry. See https://github.com/metalbear-co/mirrord/blob/main/TELEMETRY.md
    #[arg(long)]
    pub no_telemetry: bool,

    #[arg(long)]
    /// Disable version check on startup.
    pub disable_version_check: bool,

    /// Load config from config file
    #[arg(short = 'f', long)]
    pub config_file: Option<PathBuf>,
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

        /// A mirrord for Teams license key (online)
        #[arg(long)]
        license_key: Option<String>,

        /// Path to a file containing a mirrord for Teams license certificate
        #[arg(long)]
        license_path: Option<PathBuf>,

        /// Output Kubernetes specs to file instead of stdout
        #[arg(short, long)]
        file: Option<PathBuf>,

        /// Namespace to create the operator in (this doesn't limit the namespaces the operator
        /// will be able to access)
        #[arg(short, long, default_value = "mirrord")]
        namespace: OperatorNamespace,

        /// Setup operator for offline telemetry collection
        #[arg(long, hide = true)]
        offline: bool,
    },
    /// Print operator status
    Status {
        /// Specify config file to use
        #[arg(short = 'f')]
        config_file: Option<String>,
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

    /// Specify config file to use
    #[arg(short = 'f')]
    pub config_file: Option<String>,
}

#[derive(Args, Debug)]
pub(super) struct ExtensionExecArgs {
    /// Specify config file to use
    #[arg(short = 'f')]
    pub config_file: Option<String>,
    /// Specify target
    #[arg(short = 't')]
    pub target: Option<String>,
    /// User executable - the executable the layer is going to be injected to.
    #[arg(short = 'e')]
    pub executable: Option<String>,
}

#[derive(Args, Debug)]
pub(super) struct InternalProxyArgs {
    /// Launch timeout until we get first connection.
    /// If layer doesn't connect in this time, we timeout and exit.
    #[arg(short = 't', default_value_t = 30)]
    pub timeout: u64,

    /// Specify config file to use
    #[arg(short = 'f')]
    pub config_file: Option<String>,
}

#[derive(Args, Debug)]
pub(super) struct WaitlistArgs {
    /// Email to register
    #[arg(value_parser = email_parse)]
    pub email: EmailAddress,
}

fn email_parse(email: &str) -> Result<EmailAddress, String> {
    email
        .parse()
        .map_err(|e: email_address::Error| format!("invalid email address provided: {e:?}"))
}
