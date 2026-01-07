#![deny(missing_docs)]

#[cfg(not(target_os = "windows"))]
use std::os::unix::ffi::OsStringExt;
use std::{
    borrow::Cow,
    collections::HashMap,
    ffi::{OsStr, OsString},
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    str::FromStr,
};

use clap::{ArgGroup, Args, Parser, Subcommand, ValueEnum, ValueHint};
use clap_complete::Shell;
pub use mirrord_config::container::ContainerRuntime;
use mirrord_config::{
    LayerConfig, env_key,
    feature::env::{
        MIRRORD_OVERRIDE_ENV_FILE_ENV, MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE_ENV,
        MIRRORD_OVERRIDE_ENV_VARS_INCLUDE_ENV,
    },
    target::TargetType,
};
use thiserror::Error;
/// Macro to automatically handle Windows unsupported commands.
/// Usage: `windows_unsupported!(args, "command_name", { command_execution })`
#[macro_export]
macro_rules! windows_unsupported {
    ($args:expr, $command_name:literal, $block:block) => {{
        // we use cfg! to prevent rust from optimizing out $block which forces us to
        // cfg(not(windows))  existing pieces of compilable code, used but unix but
        // currently not windows,  which'll be used by windows in the future (currently
        // assumed as untested but compilable)
        if cfg!(target_os = "windows") {
            return Err($crate::error::CliError::UnsupportedOnWindows(
                $command_name.to_string(),
            ));
        }
        $block
    }};
}

#[derive(Debug, Parser)]
#[command(
    author,
    version,
    about,
    long_about = r#"
Encountered an issue? Have a feature request?
Join our Slack at https://metalbear.com/slack , create a GitHub issue at https://github.com/metalbear-co/mirrord/issues/new/choose, or email as at hi@metalbear.com"#
)]
pub(super) struct Cli {
    #[command(subcommand)]
    pub(super) commands: Commands,
}

#[derive(Debug, Subcommand)]
pub(super) enum Commands {
    /// Create and run a new container from an image with mirrord loaded (unstable).
    Container(Box<ContainerArgs>),

    /// Execute a binary using mirrord: intercept remote traffic, provide access to remote
    /// resources (network, files) and environment variables.
    Exec(Box<ExecArgs>),

    /// Print incoming tcp traffic of specific ports from remote target.
    #[cfg_attr(target_os = "windows", command(hide = true))]
    Dump(Box<DumpArgs>),

    /// Generate shell completions for the provided shell.
    /// Supported shells: bash, elvish, fish, powershell, zsh
    Completions(CompletionsArgs),

    /// Called from `mirrord exec`/`mirrord ext`.
    ///
    /// Extracts mirrord-layer lib (which is compiled into the mirrord CLI binary)
    /// to a file, so that it can be used with
    /// [`INJECTION_ENV_VAR`](crate::execution::INJECTION_ENV_VAR).
    #[command(hide = true)]
    Extract { path: String },

    /// Execute a command related to the mirrord Operator.
    #[cfg_attr(target_os = "windows", command(hide = true))]
    Operator(Box<OperatorArgs>),

    /// List available mirrord targets in the cluster.
    #[command(hide = true, name = "ls")]
    ListTargets(Box<ListTargetArgs>),

    /// Spawned by the IDE extensions.
    ///
    /// Works like [`Commands::Container`],
    /// but doesn't run the command until completion.
    ///
    /// Instead, it prepares the agent and mirrord proxies,
    /// prints data required by the extension,
    /// and waits for the external proxy to finish.
    #[command(hide = true, name = "container-ext")]
    ExtensionContainer(Box<ExtensionContainerArgs>),

    /// Spawned by the IDE extensions.
    ///
    /// Works like [`Commands::Exec`],
    /// but doesn't exec into the user command.
    ///
    /// Instead, it prepares the agent and `mirrord intproxy`,
    /// prints data required by the extension,
    /// and waits for the internal proxy to finish.
    #[command(hide = true, name = "ext")]
    ExtensionExec(Box<ExtensionExecArgs>),

    /// Spawned by [`Commands::ExtensionContainer`] or [`Commands::Container`].
    ///
    /// Acts as a proxy between the [`Commands::InternalProxy`] sidecar container
    /// and the agent/operator.
    #[command(hide = true, name = "extproxy")]
    ExternalProxy {
        /// Port on which the extproxy will accept connections.
        #[arg(long, default_value_t = 0)]
        port: u16,
        /// Debug arguments.
        ///
        /// These are passed only for visibility in `ps` output,
        /// to improve debugging experience.
        ///
        /// Should not be used from within the extproxy itself.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
        _debug_args: Vec<OsString>,
    },

    /// Spawned:
    /// 1. Natively by [`Commands::ExtensionExec`]/[`Commands::Exec`],
    /// 2. In a container by [`Commands::ExtensionContainer`]/[`Commands::Container`].
    ///
    /// Acts as a proxy between multiple mirrord-layer instances
    /// and the agent/operator.
    #[command(hide = true, name = "intproxy")]
    InternalProxy {
        /// Port on which the intproxy will accept connections.
        #[arg(long, default_value_t = 0)]
        port: u16,

        /// Set this when starting the internal proxy from `mirrord ci start`.
        ///
        /// Enables mirrord-for-ci intproxy pid saving, and checking for the `MIRRORD_CI_API_KEY`
        /// env var.
        #[arg(long, default_value_t = false)]
        mirrord_for_ci: bool,

        /// Debug arguments.
        ///
        /// These are passed only for visibility in `ps` output,
        /// to improve debugging experience.
        ///
        /// Should not be used from within the intproxy itself.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, hide = true)]
        _debug_args: Vec<OsString>,
    },

    /// Forward local ports to hosts available from the cluster
    /// or intercept traffic and direct it to local ports (unstable).
    #[command(name = "port-forward")]
    PortForward(Box<PortForwardArgs>),

    /// Manage database branching.
    #[command(name = "db-branches")]
    DbBranches(Box<DbBranchesArgs>),

    /// Verify config file without starting mirrord.
    ///
    /// Called from the IDE extensions.
    #[command(hide = true)]
    VerifyConfig(VerifyConfigArgs),

    /// Try out mirrord for Teams.
    #[cfg_attr(target_os = "windows", command(hide = true))]
    Teams,

    /// Diagnose mirrord setup.
    Diagnose(Box<DiagnoseArgs>),

    /// Run mirrord vpn (alpha).
    #[command(hide = true)]
    Vpn(Box<VpnArgs>),

    /// Subscribe to the mirrord newsletter.
    Newsletter,

    /// Execute a command related to mirrord CI.
    #[cfg_attr(target_os = "windows", command(hide = true))]
    Ci(Box<CiArgs>),

    /// Manage preview environments (requires operator).
    #[cfg_attr(target_os = "windows", command(hide = true))]
    Preview(Box<PreviewArgs>),

    /// Launch the config wizard.
    ///
    /// The config wizard is a web app that allows the user to create a mirrord config file by
    /// interacting with the GUI instead of by hand. This includes starting with a boilerplate
    /// config, finding targets in the cluster and using exposed target ports to create network
    /// configuration. Like `mirrord exec` it requires a connection to the cluster.
    #[cfg(feature = "wizard")]
    Wizard(Box<WizardArgs>),

    /// Fix issues related to mirrord.
    Fix(FixArgs),
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

impl core::fmt::Display for FsMode {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(match self {
            FsMode::Local => "local",
            FsMode::LocalWithOverrides => "localwithoverrides",
            FsMode::Read => "read",
            FsMode::Write => "write",
        })
    }
}

/// Parameters to override any values from mirrord-config as part of `exec` or `container` commands.
#[derive(Args, Debug)]
pub(super) struct ExecParams {
    /// Parameters for the target
    #[clap(flatten)]
    pub target: TargetParams,

    /// Parameters for the agent.
    #[clap(flatten)]
    pub agent: AgentParams,

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

    /// Accept/reject invalid certificates.
    #[arg(short = 'c', long, default_missing_value="true", num_args=0..=1, require_equals=true)]
    pub accept_invalid_certificates: Option<bool>,

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

    /// Disable version check on startup.
    #[arg(long)]
    pub disable_version_check: bool,

    /// Load config from config file
    /// When using -f flag without a value, defaults to "./.mirrord/mirrord.json"
    #[arg(short = 'f', long, value_hint = ValueHint::FilePath, default_missing_value = "./.mirrord/mirrord.json", num_args = 0..=1)]
    pub config_file: Option<PathBuf>,

    /// Kube context to use from Kubeconfig
    #[arg(long)]
    pub context: Option<String>,

    /// Path to env file that should be used for the execution.
    ///
    /// Allows for passing environment variables from an env file.
    ///
    /// These variables will override environment fetched from the remote target.
    #[arg(long, value_hint = ValueHint::FilePath)]
    pub env_file: Option<PathBuf>,

    /// An identifier for this mirrord session.
    ///
    /// Available as the `{{ key }}` template variable in config files.
    /// If not provided here or in the config file, a unique key is generated automatically.
    #[arg(long)]
    pub key: Option<String>,
}

impl ExecParams {
    /// Returns these parameters as an environment variables map.
    ///
    /// The map can be used when resolving the config with [`LayerConfig::resolve`].
    pub fn as_env_vars(&self) -> HashMap<&'static OsStr, Cow<'_, OsStr>> {
        let mut envs = self.agent.as_env_vars();

        envs.extend(
            self.target
                .as_env_vars()
                .into_iter()
                .map(|(key, value)| (key, Cow::Borrowed(value))),
        );

        if self.no_telemetry {
            envs.insert(
                "MIRRORD_TELEMETRY".as_ref(),
                Cow::Borrowed("false".as_ref()),
            );
        }
        if let Some(skip_processes) = &self.skip_processes {
            envs.insert(
                "MIRRORD_SKIP_PROCESSES".as_ref(),
                Cow::Borrowed(skip_processes.as_ref()),
            );
        }
        if let Some(fs_mode) = self.fs_mode {
            #[cfg(not(target_os = "windows"))]
            let file_mode = OsString::from_vec(fs_mode.to_string().into_bytes());
            #[cfg(target_os = "windows")]
            let file_mode = OsString::from(fs_mode.to_string());
            envs.insert("MIRRORD_FILE_MODE".as_ref(), Cow::Owned(file_mode));
        }
        if let Some(override_env_vars_exclude) = &self.override_env_vars_exclude {
            envs.insert(
                MIRRORD_OVERRIDE_ENV_VARS_EXCLUDE_ENV.as_ref(),
                Cow::Borrowed(override_env_vars_exclude.as_ref()),
            );
        }
        if let Some(override_env_vars_include) = &self.override_env_vars_include {
            envs.insert(
                MIRRORD_OVERRIDE_ENV_VARS_INCLUDE_ENV.as_ref(),
                Cow::Borrowed(override_env_vars_include.as_ref()),
            );
        }
        if self.no_remote_dns {
            envs.insert(
                "MIRRORD_REMOTE_DNS".as_ref(),
                Cow::Borrowed("false".as_ref()),
            );
        }
        if let Some(accept_invalid_certificates) = self.accept_invalid_certificates {
            let value = if accept_invalid_certificates {
                tracing::warn!("Accepting invalid certificates");
                "true"
            } else {
                "false"
            };

            envs.insert(
                "MIRRORD_ACCEPT_INVALID_CERTIFICATES".as_ref(),
                Cow::Borrowed(value.as_ref()),
            );
        }
        if self.tcp_steal {
            envs.insert(
                "MIRRORD_AGENT_TCP_STEAL_TRAFFIC".as_ref(),
                Cow::Borrowed("true".as_ref()),
            );
        }
        if self.no_outgoing || self.no_tcp_outgoing {
            envs.insert(
                "MIRRORD_TCP_OUTGOING".as_ref(),
                Cow::Borrowed("false".as_ref()),
            );
        }
        if self.no_outgoing || self.no_udp_outgoing {
            envs.insert(
                "MIRRORD_UDP_OUTGOING".as_ref(),
                Cow::Borrowed("false".as_ref()),
            );
        }
        if let Some(context) = &self.context {
            envs.insert(
                "MIRRORD_KUBE_CONTEXT".as_ref(),
                Cow::Borrowed(context.as_ref()),
            );
        }
        if let Some(config_file) = &self.config_file {
            envs.insert(
                LayerConfig::FILE_PATH_ENV.as_ref(),
                Cow::Borrowed(config_file.as_ref()),
            );
        }
        if let Some(env_file) = &self.env_file {
            envs.insert(
                MIRRORD_OVERRIDE_ENV_FILE_ENV.as_ref(),
                Cow::Borrowed(env_file.as_ref()),
            );
        }
        if let Some(key) = &self.key {
            envs.insert(
                env_key::MIRRORD_ENV_KEY.as_ref(),
                Cow::Borrowed(key.as_ref()),
            );
        }

        envs
    }
}

// `mirrord exec` command
#[derive(Args, Debug)]
pub(super) struct ExecArgs {
    #[clap(flatten)]
    pub params: Box<ExecParams>,

    /// Binary to execute and connect with the remote pod.
    pub binary: String,

    /// Arguments to pass to the binary.
    pub(super) binary_args: Vec<String>,
}

// `mirrord dump` command
#[derive(Args, Debug)]
pub(super) struct DumpArgs {
    #[clap(flatten)]
    pub params: Box<ExecParams>,

    /// List of ports to dump data from.
    /// Can be specified multiple times.
    #[arg(short = 'p', long)]
    pub ports: Vec<u16>,
}

// `mirrord ci start` command
#[derive(Args, Debug)]
pub(super) struct CiStartArgs {
    /// Args passed down to mirrord itself (similar to `mirrord exec`).
    #[clap(flatten)]
    pub exec_args: Box<ExecArgs>,

    /// Runs mirrord ci in the foreground (the default behaviour is to run it as a background
    /// task).
    #[arg(long)]
    pub foreground: bool,

    /// CI environment, e.g. "staging", "production", "testing", etc.
    #[arg(long)]
    pub environment: Option<String>,

    /// CI pipeline or job name, e.g. "e2e-tests".
    #[arg(long)]
    pub pipeline: Option<String>,

    /// CI pipeline trigger, e.g. "push", "pull request", "manual", etc.
    #[arg(long)]
    pub triggered_by: Option<String>,
}

/// Target-related parameters, present in more than one command.
#[derive(Args, Debug)]
pub(super) struct TargetParams {
    /// Name of the target to mirror.
    ///
    /// Valid formats:
    /// - `targetless`
    /// - `pod/{pod-name}[/container/{container-name}]`
    /// - `deployment/{deployment-name}[/container/{container-name}]`
    /// - `rollout/{rollout-name}[/container/{container-name}]`
    /// - `job/{job-name}[/container/{container-name}]`
    /// - `cronjob/{cronjob-name}[/container/{container-name}]`
    /// - `statefulset/{statefulset-name}[/container/{container-name}]`
    /// - `service/{service-name}[/container/{container-name}]`
    /// - `replicaset/{replicaset-name}[/container/{container-name}]`
    ///
    /// E.g `pod/my-pod/container/my-container`.
    #[arg(short = 't', long)]
    pub target: Option<String>,

    /// Namespace of the pod to mirror.
    ///
    /// Defaults to the user default namespace.
    #[arg(short = 'n', long)]
    pub target_namespace: Option<String>,
}

impl TargetParams {
    /// Returns these parameters as an environment variables map.
    ///
    /// The map can be used when resolving the config with [`LayerConfig::resolve`].
    pub fn as_env_vars(&self) -> HashMap<&'static OsStr, &OsStr> {
        let mut envs: HashMap<&OsStr, &OsStr> = Default::default();

        if let Some(target) = &self.target {
            envs.insert("MIRRORD_IMPERSONATED_TARGET".as_ref(), target.as_ref());
        }
        if let Some(namespace) = &self.target_namespace {
            envs.insert("MIRRORD_TARGET_NAMESPACE".as_ref(), namespace.as_ref());
        }

        envs
    }
}

/// Agent-related parameters, present in more than one command.
#[derive(Args, Debug)]
pub(super) struct AgentParams {
    /// Agent pod namespace.
    #[arg(short = 'a', long)]
    pub agent_namespace: Option<String>,

    /// Agent log level.
    #[arg(short = 'l', long)]
    pub agent_log_level: Option<String>,

    /// Agent container image.
    #[arg(short = 'i', long)]
    pub agent_image: Option<String>,

    /// TTL for the agent pod (in seconds).
    #[arg(long)]
    pub agent_ttl: Option<u16>,

    /// Timeout for agent startup (in seconds).
    #[arg(long)]
    pub agent_startup_timeout: Option<u16>,

    /// Spawn the agent in an ephemeral container.
    #[arg(short, long)]
    pub ephemeral_container: bool,
}

impl AgentParams {
    /// Returns these parameters as an environment variables map.
    ///
    /// The map can be used when resolving the config with [`LayerConfig::resolve`].
    pub fn as_env_vars(&self) -> HashMap<&'static OsStr, Cow<'_, OsStr>> {
        let mut envs: HashMap<&'static OsStr, Cow<'_, OsStr>> = Default::default();

        if let Some(namespace) = &self.agent_namespace {
            envs.insert(
                "MIRRORD_AGENT_NAMESPACE".as_ref(),
                Cow::Borrowed(namespace.as_ref()),
            );
        }
        if let Some(log_level) = &self.agent_log_level {
            envs.insert(
                "MIRRORD_AGENT_RUST_LOG".as_ref(),
                Cow::Borrowed(log_level.as_ref()),
            );
        }
        if let Some(image) = &self.agent_image {
            envs.insert(
                "MIRRORD_AGENT_IMAGE".as_ref(),
                Cow::Borrowed(image.as_ref()),
            );
        }
        if let Some(agent_ttl) = &self.agent_ttl {
            #[cfg(not(target_os = "windows"))]
            let agent_ttl = OsString::from_vec(agent_ttl.to_string().into_bytes());
            #[cfg(target_os = "windows")]
            let agent_ttl = OsString::from(agent_ttl.to_string());
            envs.insert("MIRRORD_AGENT_TTL".as_ref(), Cow::Owned(agent_ttl));
        }
        if let Some(agent_startup_timeout) = &self.agent_startup_timeout {
            #[cfg(not(target_os = "windows"))]
            let agent_startup_timeout =
                OsString::from_vec(agent_startup_timeout.to_string().into_bytes());
            #[cfg(target_os = "windows")]
            let agent_startup_timeout = OsString::from(agent_startup_timeout.to_string());
            envs.insert(
                "MIRRORD_AGENT_STARTUP_TIMEOUT".as_ref(),
                Cow::Owned(agent_startup_timeout),
            );
        }
        if self.ephemeral_container {
            envs.insert(
                "MIRRORD_EPHEMERAL_CONTAINER".as_ref(),
                Cow::Borrowed("true".as_ref()),
            );
        }

        envs
    }
}

#[derive(Args, Debug)]
#[command(group(ArgGroup::new("port-forward").args(["port_mapping", "reverse_port_mapping"]).required(true)))]
pub(super) struct PortForwardArgs {
    /// Parameters for the target.
    #[clap(flatten)]
    pub target: TargetParams,

    /// Parameters for the agent.
    #[clap(flatten)]
    pub agent: AgentParams,

    /// Whether to accept/reject invalid certificates when connecting to the Kubernetes cluster.
    #[arg(short = 'c', long, default_missing_value="true", num_args=0..=1, require_equals=true)]
    pub accept_invalid_certificates: Option<bool>,

    /// Disable telemetry - see <https://github.com/metalbear-co/mirrord/blob/main/TELEMETRY.md>.
    #[arg(long)]
    pub no_telemetry: bool,

    /// Disable version check on startup.
    #[arg(long)]
    pub disable_version_check: bool,

    /// Load config from config file.
    ///
    /// When using this argument without a value, defaults to "./.mirrord/mirrord.json"
    #[arg(short = 'f', long, value_hint = ValueHint::FilePath, default_missing_value = "./.mirrord/mirrord.json", num_args = 0..=1)]
    pub config_file: Option<PathBuf>,

    /// Kube context to use from the Kubeconfig.
    #[arg(long)]
    pub context: Option<String>,

    /// Defines port forwarding for some local port.
    ///
    /// Expected format is: `-L [local_port:]remote_ip_or_hostname:remote_port`.
    /// If the remote is given as a hostname, it is resolved lazily,
    /// after a connection is made to the local port.
    /// Local port number defaults to be the same as the remote port number.
    ///
    /// Can be used multiple times.
    #[arg(short = 'L', long, alias = "port-mappings")]
    pub port_mapping: Vec<AddrPortMapping>,

    /// Defines reverse port forwarding for some local port.
    ///
    /// Expected format is: `-R [remote_port:]local_port`.
    /// In reverse port forwarding, traffic to the remote port on the target is stolen or
    /// mirrored to local port. Remote port number defaults to be the same as the local port
    /// number.
    ///
    /// Can be used multiple times.
    #[arg(short = 'R', long)]
    pub reverse_port_mapping: Vec<PortOnlyMapping>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AddrPortMapping {
    pub local: SocketAddr,
    pub remote: (RemoteAddr, u16),
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum RemoteAddr {
    // if the remote is given as an IPv4
    Ip(Ipv4Addr),
    // if the remote needs DNS resolution, we'll delay until PortForwarder can use the
    // AgentConnection
    Hostname(String),
}

impl FromStr for AddrPortMapping {
    type Err = PortMappingParseErr;

    fn from_str(string: &str) -> Result<Self, Self::Err> {
        fn parse_port(string: &str, original: &str) -> Result<u16, PortMappingParseErr> {
            match string.parse::<u16>() {
                Ok(0) => Err(PortMappingParseErr::PortZeroInvalid(string.to_string())),
                Ok(port) => Ok(port),
                Err(_error) => Err(PortMappingParseErr::PortParseErr(
                    string.to_string(),
                    original.to_string(),
                )),
            }
        }

        fn parse_remote_addr(string: &str) -> RemoteAddr {
            string
                .parse::<Ipv4Addr>()
                .map(RemoteAddr::Ip)
                .unwrap_or(RemoteAddr::Hostname(string.to_string()))
        }

        // expected format = local_port:dest_server:remote_port
        // alternatively,  = dest_server:remote_port
        let vec: Vec<&str> = string.split(':').collect();
        let (local_port, remote_ip_str, remote_port) = match vec.as_slice() {
            [local_port, remote_ip_str, remote_port] => {
                let local_port = parse_port(local_port, string)?;
                let remote_port = parse_port(remote_port, string)?;
                (local_port, remote_ip_str, remote_port)
            }
            [remote_ip_str, remote_port] => {
                let remote_port = parse_port(remote_port, string)?;
                (remote_port, remote_ip_str, remote_port)
            }
            _ => {
                return Err(PortMappingParseErr::InvalidFormat(string.to_string()));
            }
        };
        let remote_addr = parse_remote_addr(remote_ip_str);

        Ok(Self {
            local: SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST), local_port),
            remote: (remote_addr, remote_port),
        })
    }
}

#[derive(Error, Debug, PartialEq)]
pub enum PortMappingParseErr {
    #[error(
        "Invalid format of argument `{0}`, expected `[local-port:]remote-ipv4-or-hostname:remote-port`"
    )]
    InvalidFormat(String),

    #[error("Failed to parse port `{0}` in argument `{1}`")]
    PortParseErr(String, String),

    #[error("Port `0` is not allowed in argument `{0}`")]
    PortZeroInvalid(String),
}

#[derive(Clone, Debug, PartialEq, Copy)]
pub struct PortOnlyMapping {
    pub local: LocalPort,
    pub remote: RemotePort,
}

pub type LocalPort = u16;
pub type RemotePort = u16;

impl FromStr for PortOnlyMapping {
    type Err = PortMappingParseErr;

    fn from_str(string: &str) -> Result<Self, Self::Err> {
        fn parse_port(string: &str, original: &str) -> Result<u16, PortMappingParseErr> {
            match string.parse::<u16>() {
                Ok(0) => Err(PortMappingParseErr::PortZeroInvalid(string.to_string())),
                Ok(port) => Ok(port),
                Err(_error) => Err(PortMappingParseErr::PortParseErr(
                    string.to_string(),
                    original.to_string(),
                )),
            }
        }
        // expected format = remote_port:local_port
        // alternatively,  = remote_port
        let vec: Vec<&str> = string.split(':').collect();
        let (remote, local) = match vec.as_slice() {
            [remote_port, local_port] => {
                let local_port = parse_port(local_port, string)?;
                let remote_port = parse_port(remote_port, string)?;
                (remote_port, local_port)
            }
            [remote_port] => {
                let remote_port = parse_port(remote_port, string)?;
                (remote_port, remote_port)
            }
            _ => {
                return Err(PortMappingParseErr::InvalidFormat(string.to_string()));
            }
        };
        Ok(Self { local, remote })
    }
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
    // DEPRECATED: use the helm chart instead: https://github.com/metalbear-co/charts/
    #[clap(hide(true))]
    Setup,
    /// Print operator status
    Status {
        /// Specify config file to use
        #[arg(short = 'f', long, value_hint = ValueHint::FilePath, default_missing_value = "./.mirrord/mirrord.json", num_args = 0..=1)]
        config_file: Option<PathBuf>,
    },
    /// Operator session management commands.
    ///
    /// Allows the user to forcefully kill living sessions.
    Session {
        #[command(subcommand)]
        command: SessionCommand,
        /// Load config from config file.
        /// When using -f flag without a value, defaults to "./.mirrord/mirrord.json"
        #[arg(short = 'f', long, value_hint = ValueHint::FilePath, default_missing_value = "./.mirrord/mirrord.json", num_args = 0..=1)]
        config_file: Option<PathBuf>,
    },
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
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
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

    /// Specify config file to use.
    #[arg(short = 'f', long, value_hint = ValueHint::FilePath)]
    pub config_file: Option<PathBuf>,

    /// Specify the type of target to be retrieved. If `None`, all types are retrieved.
    /// Can be used multiple times to specify multiple target types.
    #[arg(short = 't', long)]
    pub target_type: Option<Vec<TargetType>>,
}

impl ListTargetArgs {
    /// Controls the output of `mirrord ls`.
    /// If set to `true`, the command outputs a JSON object that contains more data.
    /// Otherwise, it outputs a plain array of target paths.
    pub(super) const RICH_OUTPUT_ENV: &str = "MIRRORD_LS_RICH_OUTPUT";
}

#[derive(Args, Debug)]
pub(super) struct ExtensionExecArgs {
    /// Specify config file to use
    #[arg(short = 'f', long, value_hint = ValueHint::FilePath, default_missing_value = "./mirrord.json", num_args = 0..=1)]
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
        #[arg(short = 'f', long, value_hint = ValueHint::FilePath, default_missing_value = "./.mirrord/mirrord.json", num_args = 0..=1)]
        config_file: Option<PathBuf>,
    },
}

// `mirrord container` command
#[derive(Args, Debug)]
#[clap(args_conflicts_with_subcommands = true)]
pub(super) struct ContainerArgs {
    /// Parameters to be passed to mirrord.
    #[clap(flatten)]
    pub params: Box<ExecParams>,

    /// Container command to be executed
    #[arg(trailing_var_arg = true)]
    pub exec: Vec<String>,
}

impl ContainerArgs {
    /// Unpack exec command to inner components [`RuntimeArgs`] and [`ExecParams`]
    /// (need to parse [`RuntimeArgs`] here just to make clap happy with nested trailing_var_arg)
    pub fn into_parts(self) -> (RuntimeArgs, ExecParams) {
        let ContainerArgs { params, exec } = self;

        let runtime_args = RuntimeArgs::parse_from(
            std::iter::once("mirrord container exec --".into()).chain(exec),
        );

        (runtime_args, *params)
    }
}

#[derive(Args, Debug)]
pub struct ExtensionContainerArgs {
    /// Specify config file to use
    #[arg(short = 'f', long, value_hint = ValueHint::FilePath)]
    pub config_file: Option<PathBuf>,

    /// Specify target
    #[arg(short = 't')]
    pub target: Option<String>,
}

#[derive(Parser, Debug)]
pub struct RuntimeArgs {
    /// Which kind of container runtime to use.
    #[arg(value_enum)]
    pub runtime: ContainerRuntime,

    #[command(subcommand)]
    /// Command to use with `mirrord container`.
    pub command: ContainerRuntimeCommand,
}

/// Supported command for using mirrord with container runtimes.
#[derive(Subcommand, Debug, Clone)]
pub(super) enum ContainerRuntimeCommand {
    /// Execute a `<RUNTIME> create` command with mirrord loaded. (not supported with )
    #[command(hide = true)]
    Create {
        /// Arguments that will be propogated to underlying `<RUNTIME> create` command.
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        runtime_args: Vec<String>,
    },
    /// Execute a `<RUNTIME> run` command with mirrord loaded.
    Run {
        /// Arguments that will be propogated to underlying `<RUNTIME> run` command.
        #[arg(allow_hyphen_values = true, trailing_var_arg = true)]
        runtime_args: Vec<String>,
    },
}

impl ContainerRuntimeCommand {
    pub fn create<T: Into<String>>(runtime_args: impl IntoIterator<Item = T>) -> Self {
        ContainerRuntimeCommand::Create {
            runtime_args: runtime_args.into_iter().map(T::into).collect(),
        }
    }

    pub fn has_publish(&self) -> bool {
        let runtime_args = match self {
            ContainerRuntimeCommand::Run { runtime_args } => runtime_args,
            _ => return false,
        };

        let mut hit_trailing_token = false;

        runtime_args.iter().any(|runtime_arg| {
            hit_trailing_token = hit_trailing_token || runtime_arg == "--";

            !hit_trailing_token && matches!(runtime_arg.as_str(), "-p" | "--publish")
        })
    }

    pub fn into_parts(self) -> (Vec<String>, Vec<String>) {
        match self {
            ContainerRuntimeCommand::Create { runtime_args } => {
                (vec!["create".to_owned()], runtime_args)
            }
            ContainerRuntimeCommand::Run { runtime_args } => (vec!["run".to_owned()], runtime_args),
        }
    }
}

#[derive(Args, Debug)]
pub(super) struct VpnArgs {
    /// Specify the Kubernetes namespace to vpn into.
    #[arg(short = 'n', long)]
    pub namespace: Option<String>,

    /// Load config from config file
    /// When using -f flag without a value, defaults to "./.mirrord/mirrord.json"
    #[arg(short = 'f', long, value_hint = ValueHint::FilePath, default_missing_value = "./.mirrord/mirrord.json", num_args = 0..=1)]
    pub config_file: Option<PathBuf>,

    #[cfg(target_os = "macos")]
    /// Path to resolver (macOS)
    #[arg(long, default_value = "/etc/resolver")]
    pub resolver_path: PathBuf,
}

#[derive(Args, Debug)]
pub(super) struct CiArgs {
    /// Command to use with `mirrord ci`.
    #[command(subcommand)]
    pub command: CiCommand,
}

/// `mirrord ci` commands.
#[derive(Subcommand, Debug)]
pub(super) enum CiCommand {
    /// Generates a `CiApiKey` that should be set in the ci's environment variable as
    /// `MIRRORD_CI_API_KEY`.
    ApiKey {
        /// Specify config file to use
        #[arg(short = 'f', long, value_hint = ValueHint::FilePath, default_missing_value = "./.mirrord/mirrord.json", num_args = 0..=1)]
        config_file: Option<PathBuf>,
    },
    /// Starts mirrord for ci. Takes the same arguments as `mirrord exec` plus ci specific options.
    ///
    /// - The environment variable `MIRRORD_CI_API_KEY` must be set for this command to work.
    Start(Box<CiStartArgs>),

    /// Stops mirrord for ci.
    ///
    /// - The environment variable `MIRRORD_CI_API_KEY` must be set for this command to work.
    Stop,
}

#[derive(Args, Debug)]
pub(super) struct DbBranchesArgs {
    /// Specify the namespace to operate on
    #[arg(short = 'n', long = "namespace")]
    pub namespace: Option<String>,

    /// Operate on all namespaces
    #[arg(short = 'A', long = "all-namespaces", conflicts_with = "namespace")]
    pub all_namespaces: bool,

    /// Load config from config file
    /// When using -f flag without a value, defaults to "./.mirrord/mirrord.json"
    #[arg(short = 'f', long, value_hint = ValueHint::FilePath, default_missing_value = "./.mirrord/mirrord.json", num_args = 0..=1)]
    pub config_file: Option<PathBuf>,

    #[command(subcommand)]
    pub command: DbBranchesCommand,
}

#[derive(Subcommand, Debug)]
pub(super) enum DbBranchesCommand {
    /// Show the status of database branches
    Status {
        /// Names of specific branches to show status for (all branches if none specified)
        #[arg()]
        names: Vec<String>,
    },
    /// Destroy database branches
    Destroy {
        /// Destroy all branches
        #[arg(long, conflicts_with = "names")]
        all: bool,
        /// Names of specific branches to destroy
        #[arg(required_unless_present = "all")]
        names: Vec<String>,
    },
}

#[derive(Args, Debug)]
pub struct WizardArgs {
    /// Accept/reject invalid certificates.
    #[arg(env = "MIRRORD_ACCEPT_INVALID_CERTIFICATES", short = 'c', long, default_missing_value="true", num_args=0..=1, require_equals=true
    )]
    pub accept_invalid_certificates: Option<bool>,

    /// Kube context to use from Kubeconfig.
    #[arg(env = "MIRRORD_KUBE_CONTEXT", long)]
    pub context: Option<String>,

    /// Kubeconfig.
    #[arg(env = "MIRRORD_KUBECONFIG", long)]
    pub kubeconfig: Option<String>,

    /// Controls whether mirrord sends telemetry data to MetalBear cloud. Telemetry sent doesn't
    /// contain personal identifiers or any data that should be considered sensitive. It is used to
    /// improve the product.
    /// [More information](https://github.com/metalbear-co/mirrord/blob/main/TELEMETRY.md).
    #[arg(env = "MIRRORD_TELEMETRY", long, default_value = "true")]
    pub telemetry: bool,
}

/// `mirrord fix` args.
#[derive(Args, Debug)]
pub struct FixArgs {
    /// Command to use with `mirrord fix`.
    #[command(subcommand)]
    pub command: FixCommand,
}

/// `mirrord fix` commands.
#[derive(Subcommand, Debug)]
pub enum FixCommand {
    /// Look for non-absolute paths in kubeconfig and interactively make them absolute.
    Kubeconfig(FixKubeconfig),
}

/// `mirrord fix kubeconfig` args
#[derive(Args, Debug)]
pub struct FixKubeconfig {
    /// Explicitly set path of the kubeconfig file
    #[arg(env = "MIRRORD_KUBECONFIG", long = "kubeconfig")]
    pub file_path: Option<PathBuf>,

    /// Do not make any actual changes, just print what would be changed
    #[arg(long = "dry-run")]
    pub dry_run: bool,
}

/// Arguments for `mirrord preview` command.
#[derive(Args, Debug)]
pub(super) struct PreviewArgs {
    /// Subcommand to use with `mirrord preview`.
    #[command(subcommand)]
    pub command: PreviewCommand,
}

/// `mirrord preview` subcommands.
#[derive(Subcommand, Debug)]
pub(super) enum PreviewCommand {
    /// Start a new preview environment or update an existing one.
    Start(PreviewStartArgs),
    /// Show the status of preview environments.
    Status(PreviewStatusArgs),
    /// Delete preview environments.
    Stop(PreviewStopArgs),
}

/// Arguments for `mirrord preview start` command.
#[derive(Args, Debug)]
pub(super) struct PreviewStartArgs {
    /// Container image to run in the preview pod.
    ///
    /// The image must be pre-built and pushed to a registry accessible by the cluster.
    #[arg(short = 'i', long)]
    pub image: Option<String>,

    /// Environment key to identify the preview environment.
    ///
    /// Multiple preview pods can share the same key to form a preview environment.
    /// If not provided, a new key will be generated.
    #[arg(short = 'k', long)]
    pub key: Option<String>,

    /// Target to copy configuration from.
    ///
    /// Valid formats:
    /// - `pod/{pod-name}[/container/{container-name}]`
    /// - `deployment/{deployment-name}[/container/{container-name}]`
    /// - `rollout/{rollout-name}[/container/{container-name}]`
    /// - `statefulset/{statefulset-name}[/container/{container-name}]`
    ///
    /// The preview pod will be a copy of the target's pod spec with your image.
    #[arg(short = 't', long)]
    pub target: Option<String>,

    /// Namespace of the target.
    #[arg(short = 'n', long)]
    pub target_namespace: Option<String>,

    /// Load config from config file.
    ///
    /// When using -f flag without a value, defaults to "./.mirrord/mirrord.json"
    #[arg(short = 'f', long, value_hint = ValueHint::FilePath, default_missing_value = "./.mirrord/mirrord.json", num_args = 0..=1)]
    pub config_file: Option<PathBuf>,

    /// Kube context to use from Kubeconfig.
    #[arg(long)]
    pub context: Option<String>,

    /// TTL in minutes for the preview session.
    ///
    /// The operator will terminate the session after this time elapses.
    #[arg(long)]
    pub ttl: Option<u64>,

    /// How long (in seconds) to wait for the preview to become ready.
    ///
    /// If the session hasn't reached `Ready` within this time, the CLI deletes it.
    #[arg(long)]
    pub timeout: Option<u64>,
}

impl PreviewStartArgs {
    /// Convert CLI arguments to environment variable overrides for config resolution.
    pub fn as_env_vars(&self) -> HashMap<&'static OsStr, Cow<'_, OsStr>> {
        let mut envs = HashMap::default();

        if let Some(image) = &self.image {
            envs.insert(
                "MIRRORD_PREVIEW_IMAGE".as_ref(),
                Cow::Borrowed(image.as_ref()),
            );
        }
        if let Some(key) = &self.key {
            envs.insert(
                env_key::MIRRORD_ENV_KEY.as_ref(),
                Cow::Borrowed(key.as_ref()),
            );
        }
        if let Some(target) = &self.target {
            envs.insert(
                "MIRRORD_IMPERSONATED_TARGET".as_ref(),
                Cow::Borrowed(target.as_ref()),
            );
        }
        if let Some(namespace) = &self.target_namespace {
            envs.insert(
                "MIRRORD_TARGET_NAMESPACE".as_ref(),
                Cow::Borrowed(namespace.as_ref()),
            );
        }
        if let Some(config_file) = &self.config_file {
            envs.insert(
                LayerConfig::FILE_PATH_ENV.as_ref(),
                Cow::Borrowed(config_file.as_ref()),
            );
        }
        if let Some(context) = &self.context {
            envs.insert(
                "MIRRORD_KUBE_CONTEXT".as_ref(),
                Cow::Borrowed(context.as_ref()),
            );
        }
        if let Some(ttl) = &self.ttl {
            #[cfg(not(target_os = "windows"))]
            let ttl = OsString::from_vec(ttl.to_string().into_bytes());
            #[cfg(target_os = "windows")]
            let ttl = OsString::from(ttl.to_string());
            envs.insert("MIRRORD_PREVIEW_TTL_MINS".as_ref(), Cow::Owned(ttl));
        }
        if let Some(timeout) = &self.timeout {
            #[cfg(not(target_os = "windows"))]
            let timeout = OsString::from_vec(timeout.to_string().into_bytes());
            #[cfg(target_os = "windows")]
            let timeout = OsString::from(timeout.to_string());
            envs.insert(
                "MIRRORD_PREVIEW_CREATION_TIMEOUT_SECS".as_ref(),
                Cow::Owned(timeout),
            );
        }

        envs
    }
}

/// Arguments for `mirrord preview status` command.
#[derive(Args, Debug)]
pub(super) struct PreviewStatusArgs {
    /// Filter by environment key.
    ///
    /// Can also be set via the `key` field in the mirrord config file.
    #[arg(short = 'k', long)]
    pub key: Option<String>,

    /// Namespace to query. Can also be set via `target.namespace` in the mirrord config.
    ///
    /// When neither this flag nor the config set a namespace, the command implicitly searches
    /// all namespaces (equivalent to `-A`).
    #[arg(short = 'n', long = "namespace")]
    pub namespace: Option<String>,

    /// Query all namespaces.
    #[arg(short = 'A', long = "all-namespaces", conflicts_with = "namespace")]
    pub all_namespaces: bool,

    /// Load config from config file.
    ///
    /// When using -f flag without a value, defaults to "./.mirrord/mirrord.json"
    #[arg(short = 'f', long, value_hint = ValueHint::FilePath, default_missing_value = "./.mirrord/mirrord.json", num_args = 0..=1)]
    pub config_file: Option<PathBuf>,

    /// Kube context to use from Kubeconfig.
    #[arg(long)]
    pub context: Option<String>,
}

impl PreviewStatusArgs {
    /// Convert CLI arguments to environment variable overrides for config resolution.
    pub fn as_env_vars(&self) -> HashMap<&'static OsStr, Cow<'_, OsStr>> {
        let mut envs = HashMap::default();

        if let Some(key) = &self.key {
            envs.insert(
                env_key::MIRRORD_ENV_KEY.as_ref(),
                Cow::Borrowed(key.as_ref()),
            );
        }
        if let Some(namespace) = &self.namespace {
            envs.insert(
                "MIRRORD_TARGET_NAMESPACE".as_ref(),
                Cow::Borrowed(namespace.as_ref()),
            );
        }
        if let Some(config_file) = &self.config_file {
            envs.insert(
                LayerConfig::FILE_PATH_ENV.as_ref(),
                Cow::Borrowed(config_file.as_ref()),
            );
        }
        if let Some(context) = &self.context {
            envs.insert(
                "MIRRORD_KUBE_CONTEXT".as_ref(),
                Cow::Borrowed(context.as_ref()),
            );
        }

        envs
    }
}

/// Arguments for `mirrord preview stop` command.
#[derive(Args, Debug)]
pub(super) struct PreviewStopArgs {
    /// Environment key used to filter the sessions that will be stopped.
    ///
    /// Can also be set via the `key` field in the mirrord config file.
    #[arg(short = 'k', long)]
    pub key: Option<String>,

    /// Specific target to delete (optional).
    ///
    /// If provided, only the preview session matching both the key and this target will be
    /// deleted. If not provided, all preview sessions with the given key will be deleted.
    #[arg(short = 't', long)]
    pub target: Option<String>,

    /// Namespace to search. Can also be set via `target.namespace` in the mirrord config.
    ///
    /// When neither this flag nor the config set a namespace, the command implicitly searches
    /// all namespaces (equivalent to `-A`).
    #[arg(short = 'n', long = "namespace")]
    pub namespace: Option<String>,

    /// Operate on all namespaces.
    #[arg(short = 'A', long = "all-namespaces", conflicts_with = "namespace")]
    pub all_namespaces: bool,

    /// Load config from config file.
    ///
    /// When using -f flag without a value, defaults to "./.mirrord/mirrord.json"
    #[arg(short = 'f', long, value_hint = ValueHint::FilePath, default_missing_value = "./.mirrord/mirrord.json", num_args = 0..=1)]
    pub config_file: Option<PathBuf>,

    /// Kube context to use from Kubeconfig.
    #[arg(long)]
    pub context: Option<String>,
}

impl PreviewStopArgs {
    /// Convert CLI arguments to environment variable overrides for config resolution.
    pub fn as_env_vars(&self) -> HashMap<&'static OsStr, Cow<'_, OsStr>> {
        let mut envs = HashMap::default();

        if let Some(key) = &self.key {
            envs.insert(
                env_key::MIRRORD_ENV_KEY.as_ref(),
                Cow::Borrowed(key.as_ref()),
            );
        }
        if let Some(namespace) = &self.namespace {
            envs.insert(
                "MIRRORD_TARGET_NAMESPACE".as_ref(),
                Cow::Borrowed(namespace.as_ref()),
            );
        }
        if let Some(config_file) = &self.config_file {
            envs.insert(
                LayerConfig::FILE_PATH_ENV.as_ref(),
                Cow::Borrowed(config_file.as_ref()),
            );
        }
        if let Some(context) = &self.context {
            envs.insert(
                "MIRRORD_KUBE_CONTEXT".as_ref(),
                Cow::Borrowed(context.as_ref()),
            );
        }

        envs
    }
}

#[cfg(test)]
mod tests {
    use rstest::rstest;

    use super::*;

    #[rstest]
    #[case("3030:152.37.110.132:3038", "127.0.0.1:3030", "152.37.110.132", "3038")]
    #[case("152.37.110.132:3038", "127.0.0.1:3038", "152.37.110.132", "3038")]
    fn parse_valid_mapping_ip(
        #[case] input: &str,
        #[case] expected_local: &str,
        #[case] expected_remote_addr: &str,
        #[case] expected_remote_port: &str,
    ) {
        let expected = AddrPortMapping {
            local: expected_local.parse().unwrap(),
            remote: (
                RemoteAddr::Ip(expected_remote_addr.parse().unwrap()),
                expected_remote_port.parse().unwrap(),
            ),
        };
        assert_eq!(AddrPortMapping::from_str(input).unwrap(), expected);
    }

    #[rstest]
    #[case("3030:its.a.hostname:3038", "127.0.0.1:3030", "its.a.hostname", "3038")]
    #[case("stringy.gov.biz:3038", "127.0.0.1:3038", "stringy.gov.biz", "3038")]
    fn parse_valid_mapping_hostname(
        #[case] input: &str,
        #[case] expected_local: &str,
        #[case] expected_remote_addr: &str,
        #[case] expected_remote_port: &str,
    ) {
        let expected = AddrPortMapping {
            local: expected_local.parse().unwrap(),
            remote: (
                RemoteAddr::Hostname(expected_remote_addr.to_string()),
                expected_remote_port.parse().unwrap(),
            ),
        };
        assert_eq!(AddrPortMapping::from_str(input).unwrap(), expected);
    }

    #[rstest]
    #[case("3030:152.37.110.132:3038:2027")]
    #[case("152.37.110.132:3030:3038")]
    #[case("3030:152.37.110.132:0")]
    #[case("3o3o:152.37.110.132:3o38")]
    #[case("30303030:152.37.110.132:3038")]
    #[case("")]
    #[should_panic]
    fn parse_invalid_mapping(#[case] input: &str) {
        AddrPortMapping::from_str(input).unwrap();
    }

    #[test]
    fn runtime_args_parsing() {
        let command = "mirrord container -t deploy/test podman run -it --rm debian";
        let result = Cli::parse_from(command.split(' '));

        let Commands::Container(container) = result.commands else {
            panic!("cli command didn't parse into container command, got: {result:#?}")
        };

        let (runtime_args, _) = container.into_parts();

        assert_eq!(runtime_args.runtime, ContainerRuntime::Podman);

        let ContainerRuntimeCommand::Run { runtime_args } = runtime_args.command else {
            panic!("expected run command");
        };

        assert_eq!(runtime_args, vec!["-it", "--rm", "debian"]);
    }

    #[test]
    fn runtime_args_parsing_with_seperator() {
        let command = "mirrord container -t deploy/test -- podman run -it --rm debian";
        let result = Cli::parse_from(command.split(' '));

        let Commands::Container(container) = result.commands else {
            panic!("cli command didn't parse into container command, got: {result:#?}")
        };

        let (runtime_args, _) = container.into_parts();

        assert_eq!(runtime_args.runtime, ContainerRuntime::Podman);

        let ContainerRuntimeCommand::Run { runtime_args } = runtime_args.command else {
            panic!("expected run command");
        };

        assert_eq!(runtime_args, vec!["-it", "--rm", "debian"]);
    }
}
