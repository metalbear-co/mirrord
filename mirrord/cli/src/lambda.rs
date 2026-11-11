//! The local runtime adapter for AWS Lambda targets (`mirrord lambda runtime`).
//!
//! A Lambda function's hub delivers each invocation it steals for this session as an HTTP request
//! on the invocation port. A runtime interface client (`awslambdaric`, `aws-lambda-ric`, a custom
//! `bootstrap`) can't take those: it pulls invocations from the Lambda Runtime API at
//! `AWS_LAMBDA_RUNTIME_API`. The adapter sits between the two. It runs under the layer, so its
//! listener on the invocation port is what the session steals for, and it serves the Runtime API
//! to the user's command, which it starts as its child.
//!
//! `mirrord exec` wraps the user's command in the adapter by itself when the agent reports an
//! `aws-lambda` platform (see [`wrap_command`]). An agent behind a sessions-manager that drops the
//! report still pairs, and the developer runs `mirrord exec -- mirrord lambda runtime -- <cmd>`
//! instead.

use std::{
    env,
    ffi::OsString,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    process::ExitStatus,
    sync::Arc,
};

use axum::{Router, extract::DefaultBodyLimit};
use mirrord_command::resolve_tokio_command;
use mirrord_config::{LayerConfig, feature::network::outgoing::OutgoingFilterConfig};
use mirrord_sessions_manager_client::AwsLambdaTarget;
use thiserror::Error;
use tokio::net::TcpListener;

use crate::config::LambdaRuntimeArgs;

mod invocation;
mod runtime_api;

/// The largest synchronous invocation payload Lambda accepts, which bounds both an invocation's
/// event and the runtime's response.
const MAX_PAYLOAD: usize = 6 * 1024 * 1024;

#[derive(Debug, Error)]
pub(crate) enum LambdaError {
    #[error("failed to listen on the invocation port {port}")]
    InvocationListener {
        port: u16,
        #[source]
        error: std::io::Error,
    },

    #[error("failed to listen for the local Runtime API")]
    RuntimeApiListener(#[source] std::io::Error),

    #[error("failed to start `{command}`")]
    Spawn {
        command: String,
        #[source]
        error: std::io::Error,
    },

    #[error("failed to wait for `{command}`")]
    Wait {
        command: String,
        #[source]
        error: std::io::Error,
    },

    #[error("failed to read the resolved mirrord config from the environment")]
    Config(#[source] mirrord_config::config::ConfigError),

    #[cfg(not(target_os = "windows"))]
    #[error("failed to find the mirrord binary to run the Lambda runtime adapter")]
    CurrentExe(#[source] std::io::Error),
}

/// Runs `mirrord lambda runtime`: serves invocations to the user's command until it exits, then
/// exits with its status.
pub(crate) async fn runtime_command(args: LambdaRuntimeArgs) -> Result<(), LambdaError> {
    let LambdaRuntimeArgs {
        invocation_port,
        command,
    } = args;

    // Loopback with port 0 binds don't trigger a steal subscription in the layer, so the Runtime
    // API stays a private channel between this process and its child.
    let runtime_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(LambdaError::RuntimeApiListener)?;
    let runtime_address = runtime_listener
        .local_addr()
        .map_err(LambdaError::RuntimeApiListener)?;

    let invocation_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, invocation_port))
        .await
        .map_err(|error| LambdaError::InvocationListener {
            port: invocation_port,
            error,
        })?;

    let invocations = Arc::new(runtime_api::Invocations::default());

    let runtime_router =
        runtime_api::router(invocations.clone()).layer(DefaultBodyLimit::max(MAX_PAYLOAD));
    tokio::spawn(async move {
        if let Err(error) = axum::serve(runtime_listener, runtime_router).await {
            tracing::warn!(%error, "The local Runtime API stopped");
        }
    });

    let invocation_router = Router::new()
        .fallback(invocation::handle)
        .with_state(invocations);
    tokio::spawn(async move {
        if let Err(error) = axum::serve(invocation_listener, invocation_router).await {
            tracing::warn!(%error, "The invocation listener stopped");
        }
    });

    let status = run_child(&command, runtime_address).await?;
    tracing::debug!(?status, "The local runtime exited");

    std::process::exit(exit_code(status));
}

/// Starts the runtime interface client with the environment Lambda would give it, and waits for
/// it to exit.
async fn run_child(
    command: &[OsString],
    runtime_address: SocketAddr,
) -> Result<ExitStatus, LambdaError> {
    let display = command
        .iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ");
    let (program, program_args) = command
        .split_first()
        .expect("clap requires the command to have at least one part");

    let mut child = resolve_tokio_command(program);
    child
        .args(program_args)
        .env("AWS_LAMBDA_RUNTIME_API", runtime_address.to_string())
        .kill_on_drop(true);

    // Lambda's paths for the function code and the runtime don't exist on the developer's machine,
    // and code that builds paths from them has to find the local checkout and runtime instead.
    if let Ok(task_root) = env::current_dir() {
        child.env("LAMBDA_TASK_ROOT", task_root);
    }
    if let Some(runtime_dir) = which::which(program)
        .ok()
        .and_then(|path| path.parent().map(PathBuf::from))
    {
        child.env("LAMBDA_RUNTIME_DIR", runtime_dir);
    }

    if let Some(config) = runtime_api_stays_local(runtime_address)? {
        child.env(LayerConfig::RESOLVED_CONFIG_ENV, config);
    }

    let mut child = child.spawn().map_err(|error| LambdaError::Spawn {
        command: display.clone(),
        error,
    })?;

    child.wait().await.map_err(|error| LambdaError::Wait {
        command: display,
        error,
    })
}

/// The resolved config for the child, with the Runtime API address added to the local outgoing
/// filter, or [`None`] when this process doesn't run under mirrord.
///
/// The child loads the layer too, and by default the layer sends connections to localhost through
/// the agent. The child's Runtime API calls would then reach the hub instead of this process.
fn runtime_api_stays_local(runtime_address: SocketAddr) -> Result<Option<String>, LambdaError> {
    let Ok(encoded) = env::var(LayerConfig::RESOLVED_CONFIG_ENV) else {
        return Ok(None);
    };
    let mut config = LayerConfig::decode(&encoded).map_err(LambdaError::Config)?;

    let entry = format!("tcp://{runtime_address}");
    let outgoing = &mut config.feature.network.outgoing;
    match outgoing.filter.as_mut() {
        // Everything not listed as remote is already local.
        Some(OutgoingFilterConfig::Remote(_)) => {}
        Some(OutgoingFilterConfig::Local(local)) => {
            let mut entries = local.to_vec();
            entries.push(entry);
            *local = entries.into();
        }
        None => outgoing.filter = Some(OutgoingFilterConfig::Local(vec![entry].into())),
    }

    config.encode().map(Some).map_err(LambdaError::Config)
}

#[cfg(unix)]
fn exit_code(status: ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;

    status
        .code()
        .or_else(|| status.signal().map(|signal| 128 + signal))
        .unwrap_or(1)
}

#[cfg(not(unix))]
fn exit_code(status: ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

/// The command `mirrord exec` runs for a Lambda target: this binary as the local runtime adapter,
/// with the user's command as its child.
///
/// Returns the binary to run and its arguments, starting with `argv[0]`.
#[cfg(not(target_os = "windows"))]
pub(crate) fn wrap_command(
    target: &AwsLambdaTarget,
    binary_args: Vec<String>,
) -> Result<(String, Vec<String>), LambdaError> {
    let mirrord = env::current_exe()
        .map_err(LambdaError::CurrentExe)?
        .to_string_lossy()
        .into_owned();

    let wrapped = [
        mirrord.clone(),
        "lambda".to_owned(),
        "runtime".to_owned(),
        "--invocation-port".to_owned(),
        target.invocation_port.to_string(),
        "--".to_owned(),
    ]
    .into_iter()
    .chain(binary_args)
    .collect();

    Ok((mirrord, wrapped))
}
