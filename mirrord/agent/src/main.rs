#![feature(result_option_inspect)]
#![feature(hash_drain_filter)]
#![feature(once_cell)]
#![feature(is_some_and)]
#![feature(let_chains)]
#![feature(type_alias_impl_trait)]
#![cfg_attr(target_os = "linux", feature(tcp_quickack))]

use std::{
    collections::HashMap,
    net::{Ipv4Addr, SocketAddrV4},
    path::PathBuf,
    time::Duration,
};

use cli::parse_args;
use dns::dns_worker;
use error::{AgentError, Result};
use futures::{stream::FuturesUnordered, StreamExt, TryFutureExt};
use runtime::ContainerInfo;
use sniffer::{SnifferCommand, TcpConnectionSniffer};
use tokio::{net::TcpListener, sync::mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, trace};
use tracing_subscriber::{fmt::format::FmtSpan, prelude::*};

use crate::{
    state::State,
    steal::{
        connection::TcpConnectionStealer,
        ip_tables::{
            SafeIpTables, IPTABLE_MESH, IPTABLE_MESH_ENV, IPTABLE_PREROUTING,
            IPTABLE_PREROUTING_ENV,
        },
        StealerCommand,
    },
    util::run_thread_in_namespace,
};

mod cli;
mod client;
mod dns;
mod env;
mod error;
mod file;
mod outgoing;
mod runtime;
mod sniffer;
mod state;
mod steal;
mod util;

#[cfg(not(target_os = "linux"))]
mod iptables {
    use super::*;

    pub struct IPTables;

    impl IPTables {
        pub fn new_chain(&self, _: &str, _: &str) -> Result<()> {
            unimplemented!()
        }

        pub fn flush_chain(&self, _: &str, _: &str, _: &str) -> Result<()> {
            unimplemented!()
        }

        pub fn delete_chain(&self, _: &str, _: &str, _: &str) -> Result<()> {
            unimplemented!()
        }

        pub fn append(&self, _: &str, _: &str, _: &str) -> Result<()> {
            unimplemented!()
        }

        pub fn insert(&self, _: &str, _: &str, _: &str, _: i32) -> Result<()> {
            unimplemented!()
        }
    }

    pub fn new(_: bool) -> Result<IPTables> {
        Ok(IPTables)
    }
}

const CHANNEL_SIZE: usize = 1024;

/// Initializes the agent's [`State`], channels, threads, and runs [`ClientConnectionHandler`]s.
#[tracing::instrument(level = "trace")]
async fn start_agent() -> Result<()> {
    let args = parse_args();
    trace!("Starting agent with args: {args:?}");

    let listener = TcpListener::bind(SocketAddrV4::new(
        Ipv4Addr::UNSPECIFIED,
        args.communicate_port,
    ))
    .await?;

    let mut state = State::new(&args).await?;
    let (pid, mut env) = match state.get_container_info().await? {
        Some(ContainerInfo { pid, env }) => (Some(pid), env),
        None => (None, HashMap::new()),
    };

    let environ_path = PathBuf::from("/proc")
        .join(
            pid.map(|i| i.to_string())
                .unwrap_or_else(|| "self".to_string()),
        )
        .join("environ");

    match env::get_proc_environ(environ_path).await {
        Ok(environ) => env.extend(environ.into_iter()),
        Err(err) => {
            error!("Failed to get process environment variables: {err:?}");
        }
    }

    let cancellation_token = CancellationToken::new();
    // Cancel all other tasks on exit
    let cancel_guard = cancellation_token.clone().drop_guard();

    let (sniffer_command_tx, sniffer_command_rx) = mpsc::channel::<SnifferCommand>(1000);
    let (stealer_command_tx, stealer_command_rx) = mpsc::channel::<StealerCommand>(1000);

    let (dns_sender, dns_receiver) = mpsc::channel(1000);

    let _ = run_thread_in_namespace(
        dns_worker(dns_receiver, pid),
        "DNS worker".to_string(),
        pid,
        "net",
    );

    let sniffer_cancellation_token = cancellation_token.clone();
    let sniffer_task = run_thread_in_namespace(
        TcpConnectionSniffer::new(sniffer_command_rx, args.network_interface).and_then(
            |sniffer| async move {
                if let Err(err) = sniffer.start(sniffer_cancellation_token).await {
                    error!("Sniffer failed: {err}");
                }
                Ok(())
            },
        ),
        "Sniffer".to_string(),
        pid,
        "net",
    );

    let stealer_cancellation_token = cancellation_token.clone();
    let stealer_task = run_thread_in_namespace(
        TcpConnectionStealer::new(stealer_command_rx).and_then(|stealer| async move {
            if let Err(err) = stealer.start(stealer_cancellation_token).await {
                error!("Stealer failed: {err}");
            }
            Ok(())
        }),
        "Stealer".to_string(),
        pid,
        "net",
    );

    // WARNING: This exact string is expected to be read in `pod_api.rs`, more specifically in
    // `wait_for_agent_startup`. If you change this then mirrord fails to initialize.
    println!("agent ready");

    let mut clients = FuturesUnordered::new();

    // For the first client, we use communication_timeout, then we exit when no more
    // no connections.
    match tokio::time::timeout(
        Duration::from_secs(args.communication_timeout.into()),
        listener.accept(),
    )
    .await
    {
        Ok(Ok((stream, addr))) => {
            trace!("start -> Connection accepted from {:?}", addr);
            if let Some(client) = state
                .new_connection(
                    stream,
                    sniffer_command_tx.clone(),
                    stealer_command_tx.clone(),
                    cancellation_token.clone(),
                    dns_sender.clone(),
                    args.ephemeral_container,
                    pid,
                    env.clone(),
                )
                .await?
            {
                clients.push(client)
            };
        }
        Ok(Err(err)) => {
            error!("start -> Failed to accept connection: {:?}", err);
            return Err(err.into());
        }
        Err(err) => {
            error!("start -> Failed to accept first connection: timeout");
            return Err(err.into());
        }
    }

    if args.test_error {
        return Err(AgentError::TestError);
    }

    loop {
        tokio::select! {
            Ok((stream, addr)) = listener.accept() => {
                trace!("start -> Connection accepted from {:?}", addr);
                if let Some(client) = state.new_connection(
                    stream,
                    sniffer_command_tx.clone(),
                    stealer_command_tx.clone(),
                    cancellation_token.clone(),
                    dns_sender.clone(),
                    args.ephemeral_container,
                    pid,
                    env.clone()
                ).await? {clients.push(client) };
            },
            client = clients.next() => {
                match client {
                    Some(client) => {
                        let client_id = client?;
                        state.remove_client(client_id).await?;
                    }
                    None => {
                        trace!("Main thread timeout, no clients left.");
                        break
                    }
                }
            },
        }
    }

    trace!("Agent shutting down.");
    drop(cancel_guard);

    if let Err(err) = sniffer_task.join().map_err(|_| AgentError::JoinTask)? {
        error!("start_agent -> sniffer task failed with error: {}", err);
    }

    if let Err(err) = stealer_task.join().map_err(|_| AgentError::JoinTask)? {
        error!("start_agent -> stealer task failed with error: {}", err);
    }

    trace!("Agent shutdown.");
    Ok(())
}

async fn clear_iptable_chain() -> Result<()> {
    let ipt = iptables::new(false).unwrap();

    SafeIpTables::load(ipt, false).await?.cleanup().await?;

    Ok(())
}

fn spawn_child_agent() -> Result<()> {
    let command_args = std::env::args().collect::<Vec<_>>();

    let mut child_agent = std::process::Command::new(&command_args[0])
        .args(&command_args[1..])
        .spawn()?;

    let _ = child_agent.wait();

    Ok(())
}

async fn start_iptable_guard() -> Result<()> {
    debug!("start_iptable_guard -> Initializing iptable-guard.");

    let args = parse_args();
    let state = State::new(&args).await?;
    let pid = state.get_container_info().await?.map(|c| c.pid);

    std::env::set_var(IPTABLE_PREROUTING_ENV, IPTABLE_PREROUTING.as_str());
    std::env::set_var(IPTABLE_MESH_ENV, IPTABLE_MESH.as_str());

    let result = spawn_child_agent();

    let _ = run_thread_in_namespace(
        clear_iptable_chain(),
        "clear iptables".to_owned(),
        pid,
        "net",
    )
    .join()
    .map_err(|_| AgentError::JoinTask)?;

    result
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_thread_ids(true)
                .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
                .compact(),
        )
        .with(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    debug!("main -> Initializing mirrord-agent.");

    let agent_result = if std::env::var(IPTABLE_PREROUTING_ENV).is_ok()
        && std::env::var(IPTABLE_MESH_ENV).is_ok()
    {
        start_agent().await
    } else {
        start_iptable_guard().await
    };

    match agent_result {
        Ok(_) => {
            info!("main -> mirrord-agent `start` exiting successfully.")
        }
        Err(fail) => {
            error!(
                "main -> mirrord-agent `start` exiting with error {:#?}",
                fail
            )
        }
    }

    Ok(())
}
