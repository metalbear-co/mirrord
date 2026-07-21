use std::{convert::Infallible, fmt, ops::Not, time::Duration};

use bytes::Bytes;
use futures::{
    StreamExt, future,
    stream::{self, BoxStream, SelectAll},
};
use mirrord_analytics::{AnalyticsReporter, CollectAnalytics, ExecutionKind, Reporter};
use mirrord_config::{
    LayerConfig,
    config::ConfigContext,
    target::{Target, TargetConfig},
};
use mirrord_kube::resolved::ResolvedTarget;
use mirrord_progress::{Progress, ProgressTracker};
use mirrord_protocol::{
    LogLevel, LogMessage,
    tcp::{IncomingTrafficTransportType, InternalHttpBodyFrame},
};
use mirrord_protocol_api::{
    client::{ClientError, IncomingMode, MirrordClient, TaskError},
    traffic::{BodyError, ReceivedBody, TunneledIncoming, TunneledIncomingInner},
};
use thiserror::Error;
use tokio::time::timeout;
use tracing::info;

use super::config::DumpArgs;
use crate::{
    CliError,
    connection::{ConnectData, create_and_connect},
    error::CliResult,
    kube::kube_client_from_layer_config,
    user_data::UserData,
};

/// Implements the `mirrord dump` command.
///
/// This command:
/// 1. Starts a mirrord session using the given config file and target arguments
/// 2. Subscribes to mirror traffic from the specified ports
/// 3. Prints all incoming traffic to stdout in a human friendly format
pub async fn dump_command(
    args: &DumpArgs,
    watch: drain::Watch,
    user_data: &UserData,
) -> CliResult<()> {
    // Set up configuration similar to exec command
    let mut cfg_context = ConfigContext::default().override_envs(args.params.as_env_vars());

    let mut config = LayerConfig::resolve(&mut cfg_context)?;

    let mut progress = ProgressTracker::from_env("mirrord dump");
    let mut analytics = AnalyticsReporter::new(
        config.telemetry,
        ExecutionKind::Dump,
        watch,
        user_data.machine_id(),
        Some(config.key.as_str().to_owned()),
    );

    // Ensure a target was specified
    let TargetConfig { path, namespace } = config.target.clone();
    let path: Target = match path {
        Some(Target::Targetless) | None => {
            return Err(CliError::MissingArg {
                command: "mirrord dump".to_owned(),
                arg: "--target".to_owned(),
            });
        }
        valid_target => valid_target.unwrap(),
    };

    if args.params.disable_version_check.not() {
        super::prompt_outdated_version(&progress).await;
    }
    // Collect analytics
    (&config).collect_analytics(analytics.get_mut());

    // Create connection to the agent
    let ConnectData { client, .. } =
        create_and_connect(&mut config, &mut progress, &mut analytics, None, None, None).await?;

    // If the user didn't specify ports, detect them on the target
    let ports = if args.ports.is_empty() {
        let kube_client = kube_client_from_layer_config(&config).await?;

        let resolved = ResolvedTarget::new(&kube_client, &path, namespace.as_deref())
            .await
            .map_err(|error| {
                DumpSessionError::PortDetectionFailed(format!("failed to resolve target: {error}"))
            })?;
        let pod_spec = resolved
            .resolve_pod_spec(&kube_client)
            .await
            .map_err(|error| {
                DumpSessionError::PortDetectionFailed(format!(
                    "failed to resolve target pod spec: {error}"
                ))
            })?;
        let ports: Vec<_> = pod_spec
            .map(|spec| {
                spec.containers
                    .iter()
                    .flat_map(|container| container.ports.clone().unwrap_or_default())
                    .map(|port| port.container_port.unsigned_abs() as u16)
                    .collect()
            })
            .unwrap_or_default();
        if ports.is_empty() {
            Err(DumpSessionError::PortDetectionFailed(format!(
                "no ports found in the resource spec for target {path}"
            )))?;
        }

        progress.info(
            format!("No ports were specified, attaching to all detected ports: {ports:?}").as_str(),
        );
        ports
    } else {
        args.ports.clone()
    };

    dump_session(client, ports, &mut progress).await?;

    Ok(())
}

/// Errors that can occur when dumping incoming traffic with `mirrord dump`.
#[derive(Debug, Error)]
pub enum DumpSessionError {
    #[error("agent connection failed: {0}")]
    AgentConnFailed(#[from] TaskError),

    /// Port subscriptions do not survive a reconnect to the agent,
    /// so the session cannot continue after one.
    #[error("agent connection was interrupted")]
    AgentConnInterrupted,

    #[error("subscription failed: {0}")]
    SubscriptionFailed(#[from] ClientError),

    #[error("couldn't detect ports on target, try using the `--ports` flag: {0}")]
    PortDetectionFailed(String),
}

/// Subscribes to mirror traffic on the given ports and prints it to stdout,
/// until the agent connection is lost.
async fn dump_session(
    client: MirrordClient,
    ports: Vec<u16>,
    progress: &mut ProgressTracker,
) -> Result<Infallible, DumpSessionError> {
    let mut logs = client.subscribe_logs().await?;

    let mut incoming = SelectAll::new();
    for port in ports {
        incoming.push(
            client
                .subscribe_port(port, IncomingMode::Mirror, None)
                .await?,
        );
        info!("Subscribed to port {port} for mirroring");
    }
    progress.info("Listening for traffic... Press Ctrl+C to stop");
    progress.success(Some("Subscribed to all ports"));

    let mut traffic = SelectAll::new();
    // The client keeps mirrord-protocol connection/request ids internal,
    // so we number tunnels ourselves for display.
    let mut next_id: u64 = 0;

    loop {
        tokio::select! {
            Some(LogMessage { level, message }) = logs.next() => match level {
                LogLevel::Error => tracing::error!("Received log: {message}"),
                LogLevel::Warn => tracing::warn!("Received log: {message}"),
                LogLevel::Info => tracing::info!("Received log: {message}"),
            },

            new = incoming.next() => match new {
                Some(new) => {
                    traffic.push(tunnel_events(next_id, new));
                    next_id += 1;
                }
                // Subscription streams only close when the agent connection drops.
                // Give the client task a moment to fail, so we can report the root cause.
                None => {
                    return Err(match timeout(Duration::from_secs(1), client.failed()).await {
                        Ok(error) => error.into(),
                        Err(..) => DumpSessionError::AgentConnInterrupted,
                    });
                }
            },

            Some((id, event)) = traffic.next() => print_event(id, event),
        }
    }
}

/// A printable event from a live mirrored tunnel.
enum TrafficEvent {
    ConnData(Bytes),
    ConnClosed,
    RequestFrame(InternalHttpBodyFrame),
    RequestFailed(BodyError),
    RequestFinished,
}

/// Announces new incoming traffic on stdout and returns a stream of
/// the [`TrafficEvent`]s that follow, tagged with `id`.
///
/// Mirrored traffic is read-only: the write halves of the tunnel
/// (raw data sink, HTTP response channel) are already closed, so we drop them here.
fn tunnel_events(id: u64, traffic: TunneledIncoming) -> BoxStream<'static, (u64, TrafficEvent)> {
    let TunneledIncoming {
        inner,
        transport,
        remote_local_addr,
        remote_peer_addr,
    } = traffic;

    match inner {
        TunneledIncomingInner::Raw(data) => {
            println!(
                "## New {} connection established: Connection ID {id} \
                from {remote_peer_addr} to {remote_local_addr}",
                transport_label(&transport, "TCP", "TLS"),
            );
            data.stream
                .map(TrafficEvent::ConnData)
                .chain(stream::once(future::ready(TrafficEvent::ConnClosed)))
                .map(move |event| (id, event))
                .boxed()
        }

        TunneledIncomingInner::Http(request) => {
            let request = *request.request;
            println!(
                "## New {} request received: Request ID {id} \
                from {remote_peer_addr} to {remote_local_addr}",
                transport_label(&transport, "HTTP", "HTTPS"),
            );
            println!("{}", RequestHead(&request));

            let ReceivedBody { head, tail } = request.into_body();
            stream::iter(head)
                .map(TrafficEvent::RequestFrame)
                .chain(stream::iter(tail).flatten().flat_map(|result| {
                    stream::iter(match result {
                        Ok(body) => body
                            .frames
                            .into_iter()
                            .map(TrafficEvent::RequestFrame)
                            .collect::<Vec<_>>(),
                        Err(error) => vec![TrafficEvent::RequestFailed(error)],
                    })
                }))
                .chain(stream::once(future::ready(TrafficEvent::RequestFinished)))
                .map(move |event| (id, event))
                .boxed()
        }
    }
}

fn print_event(id: u64, event: TrafficEvent) {
    match event {
        TrafficEvent::ConnData(bytes) => {
            println!("## Connection ID {id}: {} bytes", bytes.len());
            if bytes.is_empty().not() {
                // Try to print as string, fallback to hex if not valid UTF-8
                match std::str::from_utf8(&bytes) {
                    Ok(s) => println!("Data:\n{s}"),
                    Err(..) => println!("Data (hex):\n{}", hex::encode(&bytes)),
                }
            }
        }
        TrafficEvent::ConnClosed => println!("## Connection ID {id} closed"),
        TrafficEvent::RequestFrame(frame) => println!("{}", RequestFrame { id, frame }),
        TrafficEvent::RequestFailed(error) => println!("## Request ID {id} failed: {error}"),
        TrafficEvent::RequestFinished => println!("## Request ID {id} finished"),
    }
}

/// Names the protocol of a tunnel for display, e.g. "HTTP" or "HTTPS (ALPN=..., SNI=...)".
fn transport_label(transport: &IncomingTrafficTransportType, plain: &str, tls: &str) -> String {
    match transport {
        IncomingTrafficTransportType::Tcp => plain.to_owned(),
        IncomingTrafficTransportType::Tls {
            alpn_protocol,
            server_name,
        } => format!(
            "{tls} (ALPN={:?}, SNI={server_name:?})",
            alpn_protocol.as_deref().map(String::from_utf8_lossy),
        ),
    }
}

/// Provides a nice display of a request head (method, uri, version, headers).
struct RequestHead<'a, B>(&'a hyper::Request<B>);

impl<B> fmt::Display for RequestHead<'_, B> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "{} {} {:?}",
            self.0.method(),
            self.0.uri(),
            self.0.version()
        )?;
        for (header_name, header_value) in self.0.headers() {
            let value = String::from_utf8_lossy(header_value.as_bytes());
            writeln!(f, "{header_name}: {value}")?;
        }
        Ok(())
    }
}

/// Provides a nice display of a request frame.
struct RequestFrame {
    id: u64,
    frame: InternalHttpBodyFrame,
}

impl fmt::Display for RequestFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "## Request ID {} frame: ", self.id)?;

        match &self.frame {
            InternalHttpBodyFrame::Data(data) => {
                writeln!(f, "data ({} bytes)", data.len())?;
                match std::str::from_utf8(data) {
                    Ok(s) => writeln!(f, "{s}")?,
                    Err(..) => writeln!(f, "{}\n(hex)", hex::encode(data.as_ref()))?,
                }
            }
            InternalHttpBodyFrame::Trailers(trailers) => {
                writeln!(f, "trailers")?;
                for (header_name, header_value) in trailers {
                    writeln!(
                        f,
                        "{header_name}: {}",
                        String::from_utf8_lossy(header_value.as_bytes())
                    )?;
                }
            }
        }

        Ok(())
    }
}
