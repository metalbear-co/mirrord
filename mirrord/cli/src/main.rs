//! # mirrord-cli
//!
//! CLI tool for everything mirrord!
//!
//! Most of the users interact with it through the mirrord IDE plugins (which mostly just runs
//! `mirrord` commands behind the scenes).
//!
//! ## Features overview
//!
//! The main command to be aware of is `mirrord exec`, and a couple of notable ones
//! are `mirrord operator`, and `mirrord container`.
//!
//! ### `mirrord exec [OPTIONS] <BINARY> [BINARY_ARGS]`
//!
//! - [`exec`]
//!
//! > The bread and butter of mirrord.
//!
//! The `exec` command runs the user application with mirrord. You can pass pretty much any
//! binary to it that you want to run in the target context (or just in the cluster context
//! when running targetless) and it should work.
//!
//! - **Notice**: The IDEs use the `mirrord ext` command, which is very similar to `mirrord exec`,
//!   but it's fine tuned to deal with the idiosyncrasies of running mirrord from an IDE.
//!
//! - Tip: `mirrord exec -- sh` is a quick way of trying out changes.
//!
//! - Tip: You might want to use a `--` when invoking the command (e.g.`-- <BINARY> [BINARY_ARGS]`),
//!   for some binaries the command will fail to get the proper args without it.
//!
//! An `exec` run starts the [`Progress`] logging and the [`AnalyticsReporter`], prepares
//! the [`LayerConfig`], and checks it with [`LayerConfig::verify`] (which is similar to what's done
//! in the `mirrord verify-config` command).
//!
//! - Tip: [`Progress`] logging might be inconvenient sometimes when you want to see normal Rust
//!   logs, you can disable it with the `MIRRORD_PROGRESS_MODE=off` env var.
//!
//! Next, we start the target resolution, and how the target is resolved depends if the
//! mirrord-operator is available and enabled (see the section below). After [`create_and_connect`],
//! we now have the mirrord-agent information that the mirrord-internal-proxy needs. We then run
//! `mirrord intproxy` to start it, patch the user binary (macos only, see `sip_patch`),
//! and finally run the user binary with the mirrord lib loaded, but this time we use `execve`,
//! instead of [`tokio::process::Command`].
//!
//! #### operator vs no operator `exec`
//!
//! Target resolution is performed the same, regardless of operator usage, but
//! `exec` starts an agent when there is **no** operator, or the operator was explicitly
//! **disabled** in [`LayerConfig::operator`]. Otherwise, the agent creation is handled by the
//! operator, so in this case the `AgentConnectInfo` we get comes from the
//! `OperatorSessionConnection` that was assigned for this run.
//!
//! Some mirrord features and targets are only supported when the operator is being used. `exec`
//! usually stops when one of these is detected, logging an error to the user, be it in the terminal
//! or in the IDE. [`Progress`] will take care of logging using the appropriate mechanism (stderr
//! or IDE notification box with nice little buttons). For targets that might have multiple pods
//! (`deployment` and `rollout`), the user is just warned that mirrord won't impersonate all the
//! pods without the operator.
//!
//! ### `mirrord ext [OPTIONS]`
//!
//! - [`extension_exec`]
//!
//! > IDE friendly version of `mirrord exec`.
//!
//! Does pretty much the same things as `mirrord exec`, with only a few differences. [`Progress`]
//! defaults to `JsonProgress`, and it uses the `extension::mirrord_exec`, instead of [`exec`].
//!
//! You're not supposed to use this command directly from a terminal, as it might end up lacking
//! some environment variables that are set by the IDE plugins.
//!
//! ### `mirrord intproxy [OPTIONS]`
//!
//! - [`internal_proxy::proxy`]
//!
//! > Communication between mirrord-layer and mirrord-agent.
//!
//! The mirrord-intproxy is a separate process that's spawned to handle the message exchange
//! between a mirrord-layer and a mirrord-agent. The command is hidden from users, since we're the
//! ones starting the intproxy from `mirrord exec`. See the `mirrord-intproxy` crate documentation
//! for more details on the `intproxy` itself.
//!
//! It reads a previously resolved [`LayerConfig`] that has already been verified as valid, then
//! intializes logging, either to a file in `/tmp`, or to stderr when it's being started from
//! `mirrord container`.
//!
//! ### `mirrord container [OPTIONS] [EXEC]`
//!
//! - [`container_command`]
//!
//! > Runs the equivalent of `mirrord exec -- docker run {image}`.
//!
//! Running mirrord inside of a container (multiple runtimes are supported, not only docker, see
//! [`ContainerRuntime`]) requires some extra preparation than simply running `mirrord exec`.
//!
//! As with the other `mirrord exec` style commands, it starts a [`Progress`] tracker, resolves
//! [`LayerConfig`], performs target resolution and at the end starts mirrord. The big differential
//! here is that we start more than just the mirrord-intproxy and the mirrord-agent, since we now
//! also have the mirrord-extproxy.
//!
//! The mirrord-extproxy is used by the mirrord-intproxy to talk to the mirrord-agent, since the
//! internal proxy won't be able to reach the agent from within the container runtime. What it does
//! is a simplified version of the intproxy, see [`external_proxy::proxy`].
//!
//! With the external proxy running, we can get its address from stdout. We need this address when
//! starting the mirrord sidecar, which runs the `mirrord intproxy` instance that our `mirrord exec`
//! inside the user's container will connect to, something like
//! `agent<->extproxy<->intproxy<->layer` (excluding the operator from here to simplify).
//!
//! Now that we have a sidecar with intproxy (it's not running yet though), we configure the
//! `{runtime} container run` command to take into account the sidecar network, volumes, and a bunch
//! of env vars (including the `LD_PRELOAD` used to hook libmirrord). After all this is done, we
//! finally start the intproxy sidecar.
//!
//! Only then we can actually run the user's container command with mirrord, and have it working as
//! expected.
//!
//! There are actually 2 subcommands that make the whole mirrord-container experience:
//! [`ContainerRuntimeCommand::create`] that is used to prepare the sidecar, and
//! [`ContainerRuntimeCommand::Run`].
//!
//! ### `mirrord container-ext [OPTIONS]`
//!
//! - [`container_ext_command`]
//!
//! > It's to `mirrord container` what `mirrord ext` is to `mirrord exec`.
//!
//! Just as we have a special IDE favoured command in `mirrord ext`, we have an equivalent for
//! `mirrord container`, so you can run something like `mirrord exec -- docker run {image}` from an
//! IDE plugin.
//!
//! ### `mirrord extract <PATH>`
//!
//! - [`extract_library`]
//!
//! > Makes a neat `libmirrord_layer.so` file.
//!
//! The command itself is not really used anywhere. Other commands that are related to starting a
//! mirrord instance use the [`extract_library`] function directly
//!
//! ### `mirrord verify-config [OPTIONS] <PATH>`
//!
//! - [`verify_config()`]
//!
//! > Config validation.
//!
//! Performs a [`LayerConfig`] validation for the config file the user has passed, printing the
//! validated config as json (if it succeeded).
//!
//! Can be used directly from the terminal, or from an IDE plugin, but in this case we have a
//! special handling that allows the omission of a target, since in the IDE, a pop-up is shown
//! for target selection if it was missing from the [`LayerConfig`].
//!
//! ### `mirrord operator <COMMAND>`
//!
//! - [`operator_command`]
//!
//! > Setup and management of the mirrord-operator, which forms mirrord's paid offering.
//!
//! A family of commands that help managing the mirrord-operator.
//!
//! #### `mirrord operator status [OPTIONS]`
//!
//! - `StatusCommandHandler`
//!
//! Uses the `OperatorApi` to access the `/status` route in the mirrord-operator and report it to
//! the user.
//!
//! Prints a bunch of information about the mirrord operator `Session`s that are retrieved via the
//! kubernetes API in the form of the `MirrordOperatorStatus` CRD(-ish, since most of this
//! information is actually stored in the mirrord-operator itself, and not as a kubernetes
//! resource).
//!
//! Does not interact with the IDE plugins, it's a terminal only command that pretty prints this
//! information to stdout.
//!
//! #### `mirrord operator session <COMMAND>`
//!
//! - `SessionCommandHandler`
//!
//! Uses the `OperatorApi` to manage (kill) mirrord-operator sessions (`SessionSpec` CRD). It makes
//! either an `Api::delete` or an `Api::delete_collection` request through the kubernetes API.
//!
//! - Tip: to kill a particular session when you don't have its `session_id`, you can run the
//!   `mirrord operator status` command to see all the sessions.
//!
//! ### `mirrord diagnose <COMMAND>`
//!
//! - [`diagnose_command`]
//!
//! > Diagnostics for the operator.
//!
//! Currently only a network latency diagnostics check is supported.
//!
//! ### `mirrord ls [OPTIONS]`
//!
//! - [`list::print_targets`]
//!
//! > Like `ls`, but for mirrord kubernetes' targets.
//!
//! Fetches the list of supported targets from the cluster, using the `OperatorApi` if the
//! mirrord-operator is available (and has **not** been disabled in the [`LayerConfig`]), and prints
//! it back to the user. The output is used by the IDE plugins to show a nice selection box to the
//! user, when they started mirrord and have not set a target in their [`LayerConfig`].
//!
//! The types of target fetched depend on the [`ListTargetArgs::RICH_OUTPUT_ENV`].
//!
//! ### `mirrord completions <SHELL>`
//!
//! - [`generate`]
//!
//! > Completions for your shell.
//!
//! Uses [`clap`] to generate completions for the mirrord CLI.
//!
//! ### `mirrord teams`
//!
//! - [`teams::navigate_to_intro`]
//!
//! > For users interested in getting mirrord for teams, which is a paid feature.
//!
//! Opens a browser window to our mirrord for teams intro page. If we fail to open it, then it
//! prints a nice little message to stdout.
//!
//! ### `mirrord wizard [OPTIONS]`
//!
//! - `wizard::wizard_command`
//!
//! > Opens the onboarding wizard, for setting up a config file via a UI.
//!
//! Opens a browser window for the wizard. The wizard is served on `localhost` and has various
//! endpoints that are accessed by the frontend. This is all gated behind the `wizard` feature.
//!
//! ### `mirrord fix [COMMAND]`
//!
//! Detect and fix issues related to mirrord.
//! - [`fix::fix_command`]
//!
//! > Contains fixes for commonly occuring issues that prevent mirrord from working optimally.

#![feature(try_blocks)]
#![feature(iterator_try_collect)]
#![warn(clippy::indexing_slicing)]
#![deny(unused_crate_dependencies)]
#![cfg_attr(all(windows, feature = "windows_build"), feature(windows_change_time))]
#![cfg_attr(all(windows, feature = "windows_build"), feature(windows_by_handle))]

use std::{collections::HashMap, env::vars, net::SocketAddr, time::Duration};
#[cfg(not(target_os = "windows"))]
use std::{ffi::CString, os::unix::ffi::OsStrExt};
#[cfg(target_os = "macos")]
use std::{ffi::OsString, os::unix::ffi::OsStringExt};

use clap::{CommandFactory, Parser};
use clap_complete::generate;
use config::*;
use connection::create_and_connect;
use container::{container_command, container_ext_command};
use db_branches::db_branches_command;
use diagnose::diagnose_command;
use dump::dump_command;
use execution::MirrordExecution;
use extension::extension_exec;
use extract::extract_library;
use mirrord_analytics::{
    AnalyticsError, AnalyticsReporter, CollectAnalytics, ExecutionKind, Reporter,
};
use mirrord_config::{
    LayerConfig,
    config::ConfigContext,
    feature::{
        database_branches::{DatabaseBranchConfig, RedisBranchLocation},
        fs::FsModeConfig,
        network::{
            dns::{DnsConfig, DnsFilterConfig},
            incoming::IncomingMode,
        },
    },
};
use mirrord_intproxy::agent_conn::{AgentConnection, AgentConnectionError};
use mirrord_progress::{Progress, ProgressTracker, messages::EXEC_CONTAINER_BINARY};
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
use nix::errno::Errno;
use operator::operator_command;
use port_forward::{PortForwardError, PortForwarder, ReversePortForwarder};
use regex::Regex;
use semver::Version;
use tracing::{error, info, trace, warn};
use which::which;

mod browser;
mod ci;
mod config;
mod connection;
mod container;
mod db_branches;
mod diagnose;
mod dump;
mod error;
mod execution;
mod extension;
mod external_proxy;
mod extract;
mod internal_proxy;
#[cfg(target_os = "linux")]
mod is_static;
mod kube;
mod list;
mod local_redis;
mod logging;
mod newsletter;
mod operator;
mod port_forward;
mod preview;
mod profile;
mod teams;
mod user_data;
mod util;
mod verify_config;
mod vpn;
mod wsl;

#[cfg(feature = "wizard")]
mod wizard;

mod fix;

pub(crate) use error::{CliError, CliResult};
#[cfg(target_os = "windows")]
use mirrord_layer_lib::process::windows::{console, execution::LayerManagedProcess};
use verify_config::verify_config;

use crate::{
    ci::MirrordCi, newsletter::suggest_newsletter_signup, user_data::UserData,
    util::get_user_git_branch,
};

async fn exec_process<P>(
    mut config: LayerConfig,
    config_file_path: Option<&str>,
    args: &ExecArgs,
    progress: &mut P,
    analytics: &mut AnalyticsReporter,
    user_data: &mut UserData,
    mirrord_for_ci: Option<MirrordCi>,
) -> CliResult<()>
where
    P: Progress,
{
    let mut sub_progress = progress.subtask("preparing to launch process");

    #[cfg(target_os = "linux")]
    {
        use std::path::Path;

        let mut sub_progress =
            sub_progress.subtask("checking if target binary is dynamically linked");
        if is_static::is_binary_static(Path::new(&args.binary)) {
            sub_progress.failure(Some(
                "target binary might not be dynamically linked, mirrord might not work!",
            ));
        } else {
            sub_progress.success(Some("target binary is dynamically linked"));
        }
    }

    #[cfg(target_os = "macos")]
    crate::util::maybe_enable_santa_mode();

    #[cfg(target_os = "macos")]
    let binary_args = args
        .binary_args
        .iter()
        .map(|string| {
            let bytes = string.as_bytes().to_vec();
            OsString::from_vec(bytes)
        })
        .collect::<Vec<_>>();

    let execution_info = MirrordExecution::start_internal(
        &mut config,
        #[cfg(target_os = "macos")]
        Some(&args.binary),
        #[cfg(target_os = "macos")]
        Some(binary_args.as_slice()),
        &mut sub_progress,
        analytics,
        mirrord_for_ci.as_ref(),
    )
    .await?;

    // This is not being yielded, as this is not proper async, something along those lines.
    // We need an `await` somewhere in this function to drive our socket IO that happens
    // in `MirrordExecution::start`. If we don't have this here, then the websocket
    // connection resets, and in the operator you'll get a websocket error.
    tokio::time::sleep(Duration::from_micros(1)).await;

    #[cfg(target_os = "macos")]
    let (_did_sip_patch, binary) = match execution_info.patched_path {
        None => (false, args.binary.clone()),
        Some(ref sip_result) => (true, sip_result.to_owned()),
    };

    #[cfg(not(target_os = "macos"))]
    let (_did_sip_patch, binary) = (false, args.binary.clone());

    let mut env_vars: HashMap<String, String> = vars().collect();
    env_vars.extend(execution_info.environment.clone());
    env_vars.insert(mirrord_progress::MIRRORD_PROGRESS_ENV.into(), "off".into());
    for key in &execution_info.env_to_unset {
        env_vars.remove(key);
    }

    // Put original executable in argv[0] even if actually running patched version.
    let binary_args = std::iter::once(&args.binary)
        .chain(args.binary_args.iter())
        .map(Clone::clone)
        .collect::<Vec<_>>();

    sub_progress.success(Some("ready to launch process"));

    #[cfg(not(target_os = "windows"))]
    if config.experimental.browser_extension_config {
        browser::init_browser_extension(&config.feature.network, progress);
    }

    // Print config details for the user
    let mut sub_progress_config = progress.subtask("config summary");
    print_config(
        &sub_progress_config,
        Some(&binary_args),
        &config,
        config_file_path,
        execution_info.uses_operator,
    );
    // Without the success message, the final progress displays the last info message
    // as the subtask title.
    sub_progress_config.success(Some("config summary"));

    // print an invitation to the newsletter on certain run count numbers
    suggest_newsletter_signup(user_data, progress).await;

    let sub_progress = progress.subtask("running process");

    run_process_with_mirrord(
        binary,
        binary_args,
        env_vars,
        _did_sip_patch,
        sub_progress,
        analytics,
        &config,
        #[cfg(not(target_os = "windows"))]
        mirrord_for_ci,
    )
    .await
}

fn process_which(binary: &str) -> Result<std::path::PathBuf, CliError> {
    which(binary).map_err(|error| CliError::BinaryWhichError(binary.to_string(), error.to_string()))
}

#[allow(clippy::too_many_arguments)]
#[cfg(not(target_os = "windows"))]
async fn run_process_with_mirrord<P: Progress>(
    binary: String,
    binary_args: Vec<String>,
    env_vars: HashMap<String, String>,
    _did_sip_patch: bool,
    mut progress: P,
    analytics: &mut AnalyticsReporter,
    config: &LayerConfig,
    mirrord_for_ci: Option<MirrordCi>,
) -> CliResult<()> {
    // since execvpe doesn't exist on macOS, resolve path with which and use execve
    let binary_path = process_which(&binary)?;

    let path = CString::new(binary_path.as_os_str().as_bytes())?;

    let args = binary_args
        .clone()
        .into_iter()
        .map(CString::new)
        .collect::<CliResult<Vec<_>, _>>()?;

    // env vars should be formatted as "varname=value" CStrings
    let env = env_vars
        .clone()
        .into_iter()
        .map(|(k, v)| CString::new(format!("{k}={v}")))
        .collect::<CliResult<Vec<_>, _>>()?;

    progress.success(Some("Ready!"));

    match mirrord_for_ci {
        Some(mirrord_ci) => mirrord_ci
            .prepare_command(
                &mut progress,
                &binary_path,
                &binary_args,
                &env_vars,
                &config.ci,
            )
            .await
            .map_err(From::from),
        None => {
            // The execve hook is not yet active and does not hijack this call.
            let errno = nix::unistd::execve(&path, args.as_slice(), env.as_slice())
                .expect_err("call to execve cannot succeed");
            error!("Couldn't execute {:?}", errno);
            analytics.set_error(AnalyticsError::BinaryExecuteFailed);

            #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
            if errno == Errno::from_raw(86) {
                // "Bad CPU type in executable"
                if _did_sip_patch {
                    return Err(CliError::RosettaMissing(binary));
                }
            }

            if errno == nix::errno::Errno::E2BIG {
                return Err(CliError::ExecveE2Big);
            }

            Err(CliError::BinaryExecuteFailed(binary, binary_args))
        }
    }
}

#[cfg(target_os = "windows")]
async fn run_process_with_mirrord<P>(
    binary: String,
    binary_args: Vec<String>,
    env_vars: HashMap<String, String>,
    _did_sip_patch: bool,
    progress: P,
    analytics: &mut AnalyticsReporter,
    _config: &LayerConfig,
) -> CliResult<()>
where
    P: Progress,
{
    // Let Windows handle executable resolution naturally
    // Don't force .exe extension - Windows will try .exe, .bat, .cmd, etc. automatically
    let binary_name = binary.clone();

    let binary_path = process_which(&binary_name).map_err(|e| {
        error!("process_which failed: {:?}", e);
        analytics.set_error(AnalyticsError::BinaryExecuteFailed);
        e
    })?;
    let binary_path_str = binary_path.to_string_lossy().to_string();

    // Create CLI executor and configure it
    // For Windows, include the full command line with executable name
    let command_line = binary_args.join(" ");

    // spawn the process (including mirrord layer injection and wait for initialization)
    let exit_code = LayerManagedProcess::execute(
        Some(binary_path_str),
        command_line,
        // current_directory (inherit from parent)
        None,
        env_vars,
        Some(progress),
    )
    .and_then(|managed_process| managed_process.wait_until_exit())
    .map_err(|e| {
        error!("Failed to create process: {:?}", e);
        analytics.set_error(AnalyticsError::BinaryExecuteFailed);
        CliError::BinaryExecuteFailed(binary.clone(), binary_args.clone())
    })?;

    // Exit with the same code as the child process
    std::process::exit(exit_code as i32);
}

/// Prints config summary as multiple info messages, using the given [`Progress`].
pub(crate) fn print_config<P>(
    progress: &P,
    command: Option<&[String]>,
    config: &LayerConfig,
    config_file_path: Option<&str>,
    operator_used: bool,
) where
    P: Progress,
{
    if let Some(cmd) = command {
        progress.info(&format!("Running command: {}", cmd.join(" ")));
    }

    let target_and_config_path_info = format!(
        "{}, {}",
        match &config.target.path {
            Some(path) => {
                format!("mirrord will target: {}", path)
            }
            None => "mirrord will run without a target".into(),
        },
        match config_file_path {
            Some(path) => {
                format!("the configuration file was loaded from {path}")
            }
            None => "no configuration file was loaded".into(),
        }
    );
    progress.info(&target_and_config_path_info);

    let operator_info = format!(
        "mirrord will run {} the mirrord Operator",
        if operator_used { "with" } else { "without" },
    );
    progress.info(&operator_info);

    let exclude = config.feature.env.exclude.as_ref();
    let include = config.feature.env.include.as_ref();
    let env_info = if let Some(excluded) = exclude {
        if excluded.clone().to_vec().contains(&String::from("*")) {
            "no"
        } else {
            "not all"
        }
    } else if include.is_some() {
        "not all"
    } else {
        "all"
    };
    progress.info(&format!(
        "env: {} remote environment variables will be fetched",
        env_info
    ));

    let fs_info = match config.feature.fs.mode {
        FsModeConfig::Read => "read from the remote",
        FsModeConfig::Write => "read from and write to the remote",
        _ => "read and write locally",
    };
    progress.info(&format!("fs: file operations will default to {}", fs_info));

    let incoming_info = match config.feature.network.incoming.mode {
        IncomingMode::Mirror => "mirrored",
        IncomingMode::Steal => "stolen",
        IncomingMode::Off => "ignored",
    };
    progress.info(&format!(
        "incoming: incoming traffic will be {}",
        incoming_info
    ));

    let outgoing_info = match (
        config.feature.network.outgoing.tcp,
        config.feature.network.outgoing.udp,
    ) {
        (true, true) => "enabled on TCP and UDP",
        (true, false) => "enabled on TCP",
        (false, true) => "enabled on UDP",
        (false, false) => "disabled on TCP and UDP",
    };
    progress.info(&format!("outgoing: forwarding is {}", outgoing_info));

    let dns_info = match &config.feature.network.dns {
        DnsConfig { enabled: false, .. } => "locally",
        DnsConfig {
            enabled: true,
            filter: None,
        } => "remotely",
        DnsConfig {
            enabled: true,
            filter: Some(DnsFilterConfig::Remote(filters)),
        } if filters.is_empty() => "locally",
        DnsConfig {
            enabled: true,
            filter: Some(DnsFilterConfig::Local(filters)),
        } if filters.is_empty() => "remotely",
        DnsConfig {
            enabled: true,
            filter: Some(DnsFilterConfig::Remote(..)),
        } => "locally with exceptions",
        DnsConfig {
            enabled: true,
            filter: Some(DnsFilterConfig::Local(..)),
        } => "remotely with exceptions",
    };
    progress.info(&format!("dns: DNS will be resolved {}", dns_info));

    progress.info(&format!(
        "internal proxy: logs will be written to {}",
        config.internal_proxy.log_destination.display()
    ));

    progress.info(&format!("key: {}", config.key.as_str()));
}

async fn exec(
    args: &ExecArgs,
    watch: drain::Watch,
    user_data: &mut UserData,
    progress: &mut ProgressTracker,
    mirrord_for_ci: Option<MirrordCi>,
) -> CliResult<()> {
    ensure_not_nested()?;

    if !args.params.disable_version_check {
        prompt_outdated_version(progress).await;
    }
    info!(
        "Launching {:?} with arguments {:?}",
        args.binary, args.binary_args
    );

    let container_detection =
        Regex::new("docker|podman|nerdctl").expect("Failed building container detection regex!");
    if container_detection.is_match(&args.binary) {
        progress.warning(EXEC_CONTAINER_BINARY);
    }

    if !(args.params.no_tcp_outgoing || args.params.no_udp_outgoing) && args.params.no_remote_dns {
        warn!(
            "TCP/UDP outgoing enabled without remote DNS might cause issues when local machine has IPv6 enabled but remote cluster doesn't"
        )
    }

    let mut cfg_context = ConfigContext::default().override_envs(args.params.as_env_vars());
    let config_file_path = cfg_context.get_env(LayerConfig::FILE_PATH_ENV).ok();
    let mut config = LayerConfig::resolve(&mut cfg_context)?;

    crate::profile::apply_profile_if_configured(&mut config, progress).await?;

    let _local_redis: Option<local_redis::LocalRedis> = if let Some(redis_config) =
        config.feature.db_branches.iter().find_map(|branch| {
            if let DatabaseBranchConfig::Redis(redis_config) = branch
                && redis_config.location == RedisBranchLocation::Local
            {
                return Some(redis_config.clone());
            }
            None
        }) {
        let port = redis_config.local.port;

        // Get the override variable and build the appropriate connection string
        if let Some(variable) = redis_config.connection.override_variable() {
            let local_conn =
                local_redis::build_local_connection_string(port, &redis_config.connection);
            config
                .feature
                .env
                .r#override
                .get_or_insert_with(Default::default)
                .insert(variable.to_string(), local_conn);
        }

        // Auto-configure: ignore localhost so traffic goes directly to local Redis
        config.feature.network.outgoing.ignore_localhost = true;

        Some(local_redis::start(progress, &redis_config.local).await?)
    } else {
        None
    };

    let mut analytics = AnalyticsReporter::only_error(
        config.telemetry,
        Default::default(),
        watch,
        user_data.machine_id(),
    );
    (&config).collect_analytics(analytics.get_mut());

    analytics
        .get_mut()
        .add("key_length", config.key.analytics_len());

    let result = config.verify(&mut cfg_context);
    for warning in cfg_context.into_warnings() {
        progress.warning(&warning);
    }
    result?;

    let res = exec_process(
        config,
        config_file_path.as_deref(),
        args,
        progress,
        &mut analytics,
        user_data,
        mirrord_for_ci,
    )
    .await;

    if res.is_err() && !analytics.has_error() {
        analytics.set_error(AnalyticsError::Unknown);
    }
    res
}

async fn port_forward(
    args: &PortForwardArgs,
    watch: drain::Watch,
    user_data: &UserData,
) -> CliResult<()> {
    fn hash_port_mappings(
        args: &PortForwardArgs,
    ) -> CliResult<HashMap<SocketAddr, (RemoteAddr, u16)>, PortForwardError> {
        let port_mappings = &args.port_mapping;
        let mut mappings: HashMap<SocketAddr, (RemoteAddr, u16)> =
            HashMap::with_capacity(port_mappings.len());
        for mapping in port_mappings {
            if mappings
                .insert(mapping.local, mapping.remote.clone())
                .is_some()
            {
                // two mappings shared a key thus keys were not unique
                return Err(PortForwardError::PortMapSetupError(mapping.local));
            }
        }
        Ok(mappings)
    }

    fn hash_rev_port_mappings(
        args: &PortForwardArgs,
    ) -> CliResult<HashMap<RemotePort, LocalPort>, PortForwardError> {
        let port_mappings = &args.reverse_port_mapping;
        let mut mappings: HashMap<RemotePort, LocalPort> =
            HashMap::with_capacity(port_mappings.len());
        for mapping in port_mappings {
            // check destinations are unique
            if mappings.insert(mapping.remote, mapping.local).is_some() {
                // two mappings shared a key thus keys were not unique
                return Err(PortForwardError::ReversePortMapSetupError(mapping.remote));
            }
        }
        Ok(mappings)
    }

    let mut progress = ProgressTracker::from_env("mirrord port-forward");
    progress.warning("Port forwarding is currently an unstable feature and subject to change. See https://github.com/metalbear-co/mirrord/issues/2640 for more info.");

    // validate that mappings have unique local ports and reverse mappings have unique remote ports
    // before we do any more setup, keeping the hashmaps for calling PortForwarder/Reverse
    // it would be nicer to do this with clap but we're limited by the derive interface
    let port_mappings = hash_port_mappings(args)?;
    let rev_port_mappings = hash_rev_port_mappings(args)?;

    if !args.disable_version_check {
        prompt_outdated_version(&progress).await;
    }

    let mut cfg_context = ConfigContext::default()
        .override_envs(args.target.as_env_vars())
        .override_envs(args.agent.as_env_vars())
        .override_env_opt("MIRRORD_TELEMETRY", args.no_telemetry.then_some("false"))
        .override_env_opt(
            "MIRRORD_ACCEPT_INVALID_CERTIFICATES",
            args.accept_invalid_certificates.map(|accept| {
                if accept {
                    warn!("Accepting invalid certificates");
                    "true"
                } else {
                    "false"
                }
            }),
        )
        .override_env_opt("MIRRORD_KUBE_CONTEXT", args.context.as_ref())
        .override_env_opt(LayerConfig::FILE_PATH_ENV, args.config_file.as_ref());
    let mut config = LayerConfig::resolve(&mut cfg_context)?;
    crate::profile::apply_profile_if_configured(&mut config, &progress).await?;

    let mut analytics = AnalyticsReporter::new(
        config.telemetry,
        ExecutionKind::PortForward,
        watch,
        user_data.machine_id(),
    );
    (&config).collect_analytics(analytics.get_mut());

    let result = config.verify(&mut cfg_context);
    for warning in cfg_context.into_warnings() {
        progress.warning(&warning);
    }
    result?;

    let branch_name = get_user_git_branch().await;

    let (connection_info, connection) = create_and_connect(
        &mut config,
        &mut progress,
        &mut analytics,
        branch_name,
        None,
    )
    .await?;

    // errors from AgentConnection::new get mapped to CliError manually to prevent unreadably long
    // error print-outs
    let agent_conn = AgentConnection::new(&config, connection_info, &mut analytics)
        .await
        .map_err(|agent_con_error| match agent_con_error {
            AgentConnectionError::Io(error) => CliError::PortForwardingSetupError(error.into()),
            AgentConnectionError::Operator(operator_api_error) => operator_api_error.into(),
            AgentConnectionError::Kube(kube_api_error) => CliError::friendlier_error_or_else(
                kube_api_error,
                CliError::PortForwardingSetupError,
            ),
            AgentConnectionError::Tls(connection_tls_error) => connection_tls_error.into(),
            AgentConnectionError::ProtocolError(protocol_error) => protocol_error.into(),
        })?;

    let connection_2 = agent_conn.connection;

    progress.success(Some("Ready!"));
    let _ = tokio::try_join!(
        async {
            if !args.port_mapping.is_empty() {
                let mut port_forward = PortForwarder::new(connection, port_mappings).await?;
                port_forward.run().await.map_err(|error| error.into())
            } else {
                Ok::<(), CliError>(())
            }
        },
        async {
            if !args.reverse_port_mapping.is_empty() {
                let mut port_forward = ReversePortForwarder::new(
                    connection_2,
                    rev_port_mappings,
                    config.feature.network.incoming,
                    Duration::from_millis(config.experimental.idle_local_http_connection_timeout),
                )
                .await?;
                port_forward.run().await.map_err(|error| error.into())
            } else {
                Ok::<(), CliError>(())
            }
        }
    )?;

    Ok(())
}

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> miette::Result<()> {
    rustls::crypto::CryptoProvider::install_default(rustls::crypto::aws_lc_rs::default_provider())
        .expect("Failed to install crypto provider");

    // Ensure Windows consoles have VT enabled or fall back to dumb progress before we start
    // logging.
    #[cfg(target_os = "windows")]
    console::ensure_vt_or_dumb_progress();

    let cli = Cli::parse();

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(CliError::RuntimeError)?;

    let (signal, watch) = drain::channel();

    let res: CliResult<(), CliError> = rt.block_on(async move {
        logging::init_tracing_registry(&cli.commands, watch.clone()).await?;

        let mut user_data = UserData::from_default_path()
            .await
            .inspect_err(|fail| trace!(?fail, "Failed initializing `UserData`!"))
            .unwrap_or_default();

        match cli.commands {
            Commands::Exec(args) => {
                let mut progress = ProgressTracker::from_env("mirrord exec");
                exec(&args, watch, &mut user_data, &mut progress, None).await?
            }
            Commands::Dump(args) => windows_unsupported!(args, "dump", {
                dump_command(&args, watch, &user_data).await?
            }),
            Commands::Extract { path } => {
                extract_library(
                    Some(path),
                    &ProgressTracker::from_env("mirrord extract library..."),
                    false,
                )?;
            }
            Commands::ListTargets(args) => {
                let rich_output = std::env::var(ListTargetArgs::RICH_OUTPUT_ENV)
                    .ok()
                    .and_then(|value| value.parse::<bool>().ok())
                    .unwrap_or_default();

                list::print_targets(*args, rich_output).await?
            }
            Commands::Operator(args) => {
                operator_command(*args).await?;
            }
            Commands::ExtensionExec(args) => windows_unsupported!(args, "ext", {
                extension_exec(*args, watch, &user_data).await?;
            }),
            Commands::InternalProxy {
                port,
                mirrord_for_ci,
                ..
            } => {
                let config = mirrord_config::util::read_resolved_config()?;

                if mirrord_for_ci {
                    MirrordCi::prepare_intproxy().await?;
                }

                logging::init_intproxy_tracing_registry(&config).await?;
                internal_proxy::proxy(config, port, watch, &user_data).await?
            }
            Commands::VerifyConfig(args) => verify_config(args).await?,
            Commands::Completions(args) => {
                let mut cmd: clap::Command = Cli::command();
                generate(args.shell, &mut cmd, "mirrord", &mut std::io::stdout());
            }
            Commands::Teams => {
                windows_unsupported!((), "teams", { teams::navigate_to_intro().await })
            }
            Commands::Diagnose(args) => diagnose_command(*args).await?,
            Commands::Container(args) => windows_unsupported!(args, "container", {
                let (runtime_args, exec_params) = args.into_parts();

                let exit_code =
                    container_command(runtime_args, exec_params, watch, &user_data).await?;

                if exit_code != 0 {
                    std::process::exit(exit_code);
                }
            }),
            Commands::ExtensionContainer(args) => windows_unsupported!(args, "container-ext", {
                container_ext_command(args.config_file, args.target, watch, &user_data).await?
            }),
            Commands::ExternalProxy { port, .. } => windows_unsupported!(port, "extproxy", {
                let config = mirrord_config::util::read_resolved_config()?;

                logging::init_extproxy_tracing_registry(&config).await?;
                external_proxy::proxy(config, port, watch, &user_data).await?
            }),
            Commands::PortForward(args) => port_forward(&args, watch, &user_data).await?,
            Commands::Vpn(args) => {
                windows_unsupported!(args, "vpn", { vpn::vpn_command(*args).await? })
            }
            Commands::Newsletter => newsletter::newsletter_command().await,
            Commands::Ci(args) => windows_unsupported!(args, "ci", {
                ci::ci_command(*args, watch, &mut user_data).await?
            }),
            Commands::Preview(args) => windows_unsupported!(args, "preview", {
                preview::preview_command(*args, watch, &user_data).await?
            }),
            Commands::DbBranches(args) => db_branches_command(*args).await?,
            #[cfg(feature = "wizard")]
            Commands::Wizard(args) => {
                wizard::wizard_command(
                    *args,
                    watch,
                    user_data,
                    &mut ProgressTracker::from_env("wizard"),
                )
                .await?
            }
            Commands::Fix(args) => fix::fix_command(args).await?,
        };

        Ok(())
    });

    rt.block_on(async move {
        tokio::time::timeout(Duration::from_secs(10), signal.drain())
            .await
            .is_err()
            .then(|| {
                warn!("Failed to drain in a timely manner, ongoing tasks dropped.");
            });
    });

    res.map_err(Into::into)
}

/// Make sure we're not running nested inside another mirrord exec
fn ensure_not_nested() -> CliResult<()> {
    match std::env::var(mirrord_config::LayerConfig::RESOLVED_CONFIG_ENV) {
        Ok(_) => Err(CliError::NestedExec),
        Err(_) => Ok(()),
    }
}

/// Sends a request to the `analytics-server` at `/get-latest-version` to check if the mirrord
/// version being used is outdated.
///
/// We send some extra information in the query params of this request, to help us identify the
/// `source` (cli, or some IDE), `platform` (linux, macos, windows), and if we're running in ci.
async fn prompt_outdated_version(progress: &ProgressTracker) {
    let mut progress = progress.subtask("version check");
    let check_version: bool = std::env::var("MIRRORD_CHECK_VERSION")
        .map(|s| s.parse().unwrap_or(true))
        .unwrap_or(true);

    if check_version {
        let result: Result<(), Box<dyn std::error::Error>> = try {
            let client = reqwest::Client::builder()
                .user_agent(format!("mirrord-cli/{CURRENT_VERSION}"))
                .build()?;

            let sent = client
                .get(format!(
                    "https://version.mirrord.dev/get-latest-version?source=2&currentVersion={version}&platform={platform}",
                    version = CURRENT_VERSION,
                    platform = std::env::consts::OS,
                ))
                .timeout(Duration::from_secs(1))
                .send().await?;

            let latest_version = Version::parse(&sent.text().await.unwrap())?;

            if latest_version > Version::parse(CURRENT_VERSION).unwrap() {
                let is_homebrew = which("mirrord")
                    .ok()
                    .map(|mirrord_path| mirrord_path.to_string_lossy().contains("homebrew"))
                    .unwrap_or_default();
                let command = if is_homebrew {
                    "brew upgrade metalbear-co/mirrord/mirrord"
                } else {
                    "curl -fsSL https://raw.githubusercontent.com/metalbear-co/mirrord/main/scripts/install.sh | bash"
                };
                progress.print(&format!(
                    "New mirrord version available: {latest_version}. To update, run: `{command}`."
                ));
                progress.print(
                    "To disable version checks, set env variable MIRRORD_CHECK_VERSION to 'false'.",
                );
                progress.success(Some(&format!("update to {latest_version} available")));
            } else {
                progress.success(Some("running on latest!"));
            }
        };

        result.ok();
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use rstest::rstest;

    use crate::{Cli, Commands};

    /// Verifies that
    /// [`ExecParams::accept_invalid_certificates`](crate::config::ExecParams::accept_invalid_certificates)
    /// and [`PortForwardArgs::accept_invalid_certificates`](crate::config::PortForwardArgs::accept_invalid_certificates)
    /// correctly parse from command line arguments.
    #[rstest]
    #[case(&["mirrord", "exec", "-c", "--", "echo", "hello"], Some(true))]
    #[case(&["mirrord", "exec", "-c=true", "--", "echo", "hello"], Some(true))]
    #[case(&["mirrord", "exec", "-c=false", "--", "echo", "hello"], Some(false))]
    #[case(&["mirrord", "exec", "--", "echo", "hello"], None)]
    #[case(&["mirrord", "port-forward", "-c", "-L", "8080:py-serv:80"], Some(true))]
    #[case(&["mirrord", "port-forward", "-c=true", "-L", "8080:py-serv:80"], Some(true))]
    #[case(&["mirrord", "port-forward", "-c=false", "-L", "8080:py-serv:80"], Some(false))]
    #[case(&["mirrord", "port-forward", "-L", "8080:py-serv:80"], None)]
    fn parse_accept_invalid_certificates(
        #[case] args: &[&str],
        #[case] expected_value: Option<bool>,
    ) {
        match Cli::parse_from(args).commands {
            Commands::Exec(params) if *args.get(1).unwrap() == "exec" => {
                assert_eq!(params.params.accept_invalid_certificates, expected_value)
            }
            Commands::PortForward(params) if *args.get(1).unwrap() == "port-forward" => {
                assert_eq!(params.accept_invalid_certificates, expected_value)
            }
            other => panic!("unexpected args parsed: {other:?}"),
        }
    }
}
