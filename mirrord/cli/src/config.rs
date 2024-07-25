#![deny(missing_docs)]

use std::{collections::HashMap, ffi::OsString, fmt::Display, path::PathBuf, str::FromStr};

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum, ValueHint};
use clap_complete::Shell;
use mirrord_operator::setup::OperatorNamespace;

use crate::error::{CliError, UnsupportedRuntimeVariant};

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

    /// Generates shell completions for the provided shell.
    /// Supported shells: bash, elvish, fish, powershell, zsh
    Completions(CompletionsArgs),

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
    InternalProxy,

    /// Verify config file without starting mirrord.
    #[command(hide = true)]
    VerifyConfig(VerifyConfigArgs),

    /// Try out mirrord for Teams.
    Teams,

    /// Diagnostic commands
    Diagnose(Box<DiagnoseArgs>),

    /// Create and run a new container from an image with mirrord loaded
    Container(Box<ContainerArgs>),

    #[command(hide = true, name = "extproxy")]
    ExternalProxy,
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

impl Display for FsMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            FsMode::Local => "local",
            FsMode::LocalWithOverrides => "localwithoverrides",
            FsMode::Read => "read",
            FsMode::Write => "write",
        })
    }
}

#[derive(Args, Debug)]
pub(super) struct ExecParams {
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

    /// Use an Ephemeral Container to mirror traffic.
    #[arg(short, long)]
    pub ephemeral_container: bool,

    /// Steal TCP instead of mirroring
    #[arg(long = "steal")]
    pub tcp_steal: bool,

    /// Disable tcp/udp outgoing traffic
    #[arg(long)]
    pub no_outgoing: bool,

    /// Disable tcp outgoing feature.
    #[arg(long)]
    pub no_tcp_outgoing: bool,

    /// Disable udp outgoing feature.
    #[arg(long)]
    pub no_udp_outgoing: bool,

    /// Disable telemetry. See <https://github.com/metalbear-co/mirrord/blob/main/TELEMETRY.md>
    #[arg(long)]
    pub no_telemetry: bool,

    #[arg(long)]
    /// Disable version check on startup.
    pub disable_version_check: bool,

    /// Load config from config file
    #[arg(short = 'f', long, value_hint = ValueHint::FilePath)]
    pub config_file: Option<PathBuf>,

    /// Kube context to use from Kubeconfig
    #[arg(long)]
    pub context: Option<String>,
}

impl ExecParams {
    pub fn to_env(&self) -> Result<HashMap<String, OsString>, CliError> {
        let mut envs: HashMap<String, OsString> = HashMap::new();

        if let Some(target) = &self.target {
            envs.insert("MIRRORD_IMPERSONATED_TARGET".into(), target.into());
        }

        if self.no_telemetry {
            envs.insert("MIRRORD_TELEMETRY".into(), "false".into());
        }

        if let Some(skip_processes) = &self.skip_processes {
            envs.insert("MIRRORD_SKIP_PROCESSES".into(), skip_processes.into());
        }

        if let Some(namespace) = &self.target_namespace {
            envs.insert("MIRRORD_TARGET_NAMESPACE".into(), namespace.into());
        }

        if let Some(namespace) = &self.agent_namespace {
            envs.insert("MIRRORD_AGENT_NAMESPACE".into(), namespace.into());
        }

        if let Some(log_level) = &self.agent_log_level {
            envs.insert("MIRRORD_AGENT_RUST_LOG".into(), log_level.into());
        }

        if let Some(image) = &self.agent_image {
            envs.insert("MIRRORD_AGENT_IMAGE".into(), image.into());
        }

        if let Some(agent_ttl) = &self.agent_ttl {
            envs.insert("MIRRORD_AGENT_TTL".into(), agent_ttl.to_string().into());
        }
        if let Some(agent_startup_timeout) = &self.agent_startup_timeout {
            envs.insert(
                "MIRRORD_AGENT_STARTUP_TIMEOUT".into(),
                agent_startup_timeout.to_string().into(),
            );
        }

        if let Some(fs_mode) = self.fs_mode {
            envs.insert("MIRRORD_FILE_MODE".into(), fs_mode.to_string().into());
        }

        if let Some(override_env_vars_exclude) = &self.override_env_vars_exclude {
            envs.insert(
                "MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE".into(),
                override_env_vars_exclude.into(),
            );
        }

        if let Some(override_env_vars_include) = &self.override_env_vars_include {
            envs.insert(
                "MIRRORD_OVERRIDE_ENV_VARS_INCLUDE".into(),
                override_env_vars_include.into(),
            );
        }

        if self.no_remote_dns {
            envs.insert("MIRRORD_REMOTE_DNS".into(), "false".into());
        }

        if self.accept_invalid_certificates {
            envs.insert("MIRRORD_ACCEPT_INVALID_CERTIFICATES".into(), "true".into());
            tracing::warn!("Accepting invalid certificates");
        }

        if self.ephemeral_container {
            envs.insert("MIRRORD_EPHEMERAL_CONTAINER".into(), "true".into());
        };

        if self.tcp_steal {
            envs.insert("MIRRORD_AGENT_TCP_STEAL_TRAFFIC".into(), "true".into());
        };

        if self.no_outgoing || self.no_tcp_outgoing {
            envs.insert("MIRRORD_TCP_OUTGOING".into(), "false".into());
        }

        if self.no_outgoing || self.no_udp_outgoing {
            envs.insert("MIRRORD_UDP_OUTGOING".into(), "false".into());
        }

        if let Some(context) = &self.context {
            envs.insert("MIRRORD_KUBE_CONTEXT".into(), context.into());
        }

        if let Some(config_file) = &self.config_file {
            // Set canoncialized path to config file, in case forks/children are in different
            // working directories.
            let full_path = std::fs::canonicalize(config_file)
                .map_err(|e| CliError::CanonicalizeConfigPathFailed(config_file.clone(), e))?;
            envs.insert(
                "MIRRORD_CONFIG_FILE".into(),
                full_path.as_os_str().to_owned(),
            );
        }

        Ok(envs)
    }
}

#[derive(Args, Debug)]
pub(super) struct ExecArgs {
    #[clap(flatten)]
    pub params: ExecParams,

    /// Binary to execute and connect with the remote pod.
    pub binary: String,

    /// Arguments to pass to the binary.
    pub(super) binary_args: Vec<String>,
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
        /// ToS can be read here <https://metalbear.co/legal/terms>
        #[arg(long)]
        accept_tos: bool,

        /// A mirrord for Teams license key (online)
        #[arg(long, allow_hyphen_values(true))]
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
    },
    /// Print operator status
    Status {
        /// Specify config file to use
        #[arg(short = 'f', long, value_hint = ValueHint::FilePath)]
        config_file: Option<PathBuf>,
    },
    /// Operator session management commands.
    ///
    /// Allows the user to forcefully kill living sessions.
    #[command(subcommand)]
    Session(SessionCommand),
}

/// `mirrord operator session` family of commands.
///
/// Allows the user to forcefully kill operator sessions, use with care!
///
/// Implements [`core::fmt::Display`] to show the user a nice message.
#[derive(Debug, Subcommand, Clone, Copy)]
pub(crate) enum SessionCommand {
    /// Kills the session specified by `id`.
    Kill {
        /// Id of the session.
        #[arg(short, long, value_parser=hex_id)]
        id: u64,
    },
    /// Kills all operator sessions.
    KillAll,

    /// Kills _inactive_ sessions, might be useful if an undead session is still being stored in
    /// the session storage.
    #[clap(hide(true))]
    RetainActive,
}

impl core::fmt::Display for SessionCommand {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SessionCommand::Kill { id } => write!(f, "mirrord operator kill --id {id}"),
            SessionCommand::KillAll => write!(f, "mirrord operator kill-all"),
            SessionCommand::RetainActive => write!(f, "mirrord operator retain-active"),
        }
    }
}

/// Parses the operator session id from hex (without `0x` prefix) into `u64`.
fn hex_id(raw: &str) -> Result<u64, String> {
    u64::from_str_radix(raw, 16)
        .map_err(|fail| format!("Failed parsing hex session id value with {fail}!"))
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
    #[arg(short = 'f', long, value_hint = ValueHint::FilePath)]
    pub config_file: Option<PathBuf>,
}

#[derive(Args, Debug)]
pub(super) struct ExtensionExecArgs {
    /// Specify config file to use
    #[arg(short = 'f', long, value_hint = ValueHint::FilePath)]
    pub config_file: Option<PathBuf>,
    /// Specify target
    #[arg(short = 't')]
    pub target: Option<String>,
    /// User executable - the executable the layer is going to be injected to.
    #[arg(short = 'e')]
    pub executable: Option<String>,
}

/// Args for the [`mod@super::verify_config`] mirrord-cli command.
#[derive(Args, Debug)]
#[command(group(ArgGroup::new("verify-config")))]
pub(super) struct VerifyConfigArgs {
    /// Config file path.
    #[arg(long)]
    pub(super) ide: bool,

    /// Config file path.
    pub(super) path: PathBuf,
}

#[derive(Args, Debug)]
pub(super) struct CompletionsArgs {
    pub(super) shell: Shell,
}

#[derive(Args, Debug)]
pub(super) struct DiagnoseArgs {
    #[command(subcommand)]
    pub command: DiagnoseCommand,
}

#[derive(Subcommand, Debug)]
/// Commands for diagnosing potential issues introduced by mirrord.
pub(super) enum DiagnoseCommand {
    /// Check network connectivity and provide RTT (latency) statistics.
    Latency {
        /// Specify config file to use
        #[arg(short = 'f', long, value_hint = ValueHint::FilePath)]
        config_file: Option<PathBuf>,
    },
}

#[derive(Clone, Copy, Debug)]
pub(super) enum ContainerRuntime {
    Docker,
    Podman,
}

impl core::fmt::Display for ContainerRuntime {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ContainerRuntime::Docker => write!(f, "docker"),
            ContainerRuntime::Podman => write!(f, "podman"),
        }
    }
}

impl FromStr for ContainerRuntime {
    type Err = UnsupportedRuntimeVariant;

    fn from_str(runtime: &str) -> Result<Self, Self::Err> {
        match runtime {
            "docker" => Ok(ContainerRuntime::Docker),
            "podman" => Ok(ContainerRuntime::Podman),
            _ => Err(UnsupportedRuntimeVariant),
        }
    }
}

#[derive(Args, Debug)]
pub(super) struct ContainerArgs {
    #[arg(long, default_value = "localhost/mirrord-cli:latest")]
    pub cli_image: String,

    #[arg(long, default_value = "/opt/mirrord/lib/libmirrord_layer.so")]
    pub cli_image_lib_path: PathBuf,

    #[clap(flatten)]
    pub params: ExecParams,

    pub runtime: ContainerRuntime,

    #[command(subcommand)]
    pub command: ContainerCommand,
}

#[derive(Subcommand, Debug, Clone)]
pub(super) enum ContainerCommand {
    Run {
        #[arg(raw = true)]
        runtime_args: Vec<String>,
    },
}
