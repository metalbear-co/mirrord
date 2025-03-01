use std::{future, io, path::PathBuf, sync::atomic::Ordering, time::Duration};

use futures::{stream::FuturesOrdered, StreamExt};
use hickory_resolver::{
    config::{LookupIpStrategy, ServerOrderingStrategy},
    error::{ResolveError, ResolveErrorKind},
    lookup_ip::LookupIp,
    proto::error::ProtoErrorKind,
    system_conf::parse_resolv_conf,
    Hosts, TokioAsyncResolver,
};
use mirrord_agent_env::envs;
use mirrord_protocol::{
    dns::{
        AddressFamily, DnsLookup, GetAddrInfoRequest, GetAddrInfoRequestV2, GetAddrInfoResponse,
        LookupRecord,
    },
    DnsLookupError, ResolveErrorKindInternal, ResponseError,
};
use thiserror::Error;
use tokio::{
    fs,
    sync::{
        mpsc::{Receiver, Sender},
        oneshot,
    },
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;
use tracing::{warn, Level};

use crate::{
    error::{AgentError, AgentResult},
    metrics::DNS_REQUEST_COUNT,
    watched_task::TaskStatus,
};

#[derive(Debug)]
pub(crate) enum ClientGetAddrInfoRequest {
    V1(GetAddrInfoRequest),
    V2(GetAddrInfoRequestV2),
}

impl ClientGetAddrInfoRequest {
    pub(crate) fn into_v2(self) -> GetAddrInfoRequestV2 {
        match self {
            ClientGetAddrInfoRequest::V1(old_req) => old_req.into(),
            ClientGetAddrInfoRequest::V2(v2_req) => v2_req,
        }
    }
}

/// Sent from per-client [`DnsApi`] to the global [`DnsWorker`].
#[derive(Debug)]
pub(crate) struct DnsCommand {
    request: ClientGetAddrInfoRequest,
    response_tx: oneshot::Sender<Result<DnsLookup, InternalLookupError>>,
}

/// Background task for resolving hostnames to IP addresses.
/// Should be run in the same network namespace as the agent's target.
pub(crate) struct DnsWorker {
    /// Path to the `/etc` directory in the target container filesystem.
    ///
    /// This directory contains some configuration files we need
    /// to prepare the [`TokioAsyncResolver`].
    etc_path: PathBuf,
    /// For receiving [`DnsCommand`]s from [`DnsApi`]s.
    request_rx: Receiver<DnsCommand>,
    /// Max request attempts per DNS query.
    ///
    /// Configured via [`envs::DNS_ATTEMPTS`].
    attempts: Option<usize>,
    /// DNS query timeout.
    ///
    /// Configured via [`envs::DNS_TIMEOUT`].
    timeout: Option<Duration>,
    /// Whether we want support querying for IPv6 addresses.
    support_ipv6: bool,
    /// Background tasks that handle the DNS requests.
    ///
    /// Each of these builds a new [`TokioAsyncResolver`] and performs one lookup.
    tasks: JoinSet<()>,
}

impl DnsWorker {
    pub const TASK_NAME: &'static str = "DNS worker";

    /// Creates a new instance of this worker.
    /// To run this worker, call [`Self::run`].
    ///
    /// # Note
    ///
    /// `pid` is used to find the correct path of `etc` directory.
    pub(crate) fn new(
        pid: Option<u64>,
        request_rx: Receiver<DnsCommand>,
        support_ipv6: bool,
    ) -> Self {
        let etc_path = pid
            .map(|pid| {
                PathBuf::from("/proc")
                    .join(pid.to_string())
                    .join("root/etc")
            })
            .unwrap_or_else(|| PathBuf::from("/etc"));

        let timeout = envs::DNS_TIMEOUT
            .try_from_env()
            .ok()
            .flatten()
            .map(u64::from)
            .map(Duration::from_secs);
        let attempts = envs::DNS_ATTEMPTS
            .try_from_env()
            .ok()
            .flatten()
            .map(|attempts| usize::try_from(attempts).unwrap_or(usize::MAX));

        Self {
            etc_path,
            request_rx,
            timeout,
            attempts,
            support_ipv6,
            tasks: Default::default(),
        }
    }

    /// Reads `/etc/resolv.conf` and `/etc/hosts` files, then uses [`TokioAsyncResolver`] to
    /// resolve address of the given `host`.
    ///
    /// # TODO
    ///
    /// We could probably cache results here.
    /// We cannot cache the [`TokioAsyncResolver`] itself, becaues the configuration in `etc` may
    /// change.
    #[tracing::instrument(level = Level::TRACE, ret, err(level = Level::TRACE))]
    async fn do_lookup(
        etc_path: PathBuf,
        request: GetAddrInfoRequestV2,
        attempts: Option<usize>,
        timeout: Option<Duration>,
        support_ipv6: bool,
    ) -> Result<DnsLookup, InternalLookupError> {
        // Prepares the `Resolver` after reading some `/etc` DNS files.
        //
        // We care about logging these errors, at an `error!` level.
        let resolver: Result<_, InternalLookupError> = try {
            let resolv_conf_path = etc_path.join("resolv.conf");
            let hosts_path = etc_path.join("hosts");

            let resolv_conf = fs::read(resolv_conf_path).await?;
            let hosts_conf = fs::read(hosts_path).await?;

            let (config, mut options) = parse_resolv_conf(resolv_conf)?;
            tracing::debug!(?config, ?options, "Parsed resolv configuration");

            options.server_ordering_strategy = ServerOrderingStrategy::UserProvidedOrder;
            if let Some(timeout) = timeout {
                options.timeout = timeout;
            }
            if let Some(attempts) = attempts {
                options.attempts = attempts;
            }
            options.ip_strategy = if support_ipv6 {
                tracing::debug!(
                    "IPv6 support enabled. Respecting client IP family when resolving DNS."
                );
                request.family.convert()
            } else {
                tracing::debug!("IPv6 support disabled. Resolving IPv4 only.");
                LookupIpStrategy::Ipv4Only
            };

            tracing::debug!(?config, ?options, "Updated resolv configuration");

            let mut resolver = TokioAsyncResolver::tokio(config, options);
            tracing::debug!(?resolver, "Build a DNS resolver");

            let hosts = Hosts::default().read_hosts_conf(hosts_conf.as_slice())?;
            resolver.set_hosts(Some(hosts));

            resolver
        };

        let lookup = resolver
            .inspect_err(|fail| tracing::error!(?fail, "Failed to build a DNS resolver"))?
            .lookup_ip(request.node)
            .await
            .inspect(|lookup| tracing::trace!(?lookup, "DNS lookup finished"))
            .inspect_err(|e| tracing::debug!(%e, "DNS lookup failed"))?
            .convert();

        Ok(lookup)
    }

    /// Handles the given [`DnsCommand`] in a separate [`tokio::task`].
    #[tracing::instrument(level = Level::TRACE, skip(self))]
    fn handle_message(&mut self, message: DnsCommand) {
        let etc_path = self.etc_path.clone();
        let timeout = self.timeout;
        let attempts = self.attempts;
        let support_ipv6 = self.support_ipv6;

        let lookup_future = async move {
            let result = Self::do_lookup(
                etc_path,
                message.request.into_v2(),
                attempts,
                timeout,
                support_ipv6,
            )
            .await;

            let _ = message.response_tx.send(result);
        };

        DNS_REQUEST_COUNT.fetch_add(1, Ordering::Relaxed);
        self.tasks.spawn(lookup_future);
    }

    pub(crate) async fn run(mut self, cancellation_token: CancellationToken) -> AgentResult<()> {
        loop {
            tokio::select! {
                _ = cancellation_token.cancelled() => break Ok(()),

                _ = self.tasks.join_next() => {
                    DNS_REQUEST_COUNT.fetch_sub(1, Ordering::Relaxed);
                }

                message = self.request_rx.recv() => match message {
                    None => break Ok(()),
                    Some(message) => self.handle_message(message),
                },
            }
        }
    }
}

impl Drop for DnsWorker {
    fn drop(&mut self) {
        let dropped_tasks = self.tasks.len();
        DNS_REQUEST_COUNT.fetch_sub(dropped_tasks, Ordering::Relaxed);
    }
}

pub(crate) struct DnsApi {
    task_status: TaskStatus,
    request_tx: Sender<DnsCommand>,
    /// [`DnsWorker`] processes all requests concurrently, so we use a combination of [`oneshot`]
    /// channels and [`FuturesOrdered`] to preserve order of responses.
    responses: FuturesOrdered<oneshot::Receiver<Result<DnsLookup, InternalLookupError>>>,
}

impl DnsApi {
    pub(crate) fn new(task_status: TaskStatus, task_sender: Sender<DnsCommand>) -> Self {
        Self {
            task_status,
            request_tx: task_sender,
            responses: Default::default(),
        }
    }

    /// Schedules a new DNS request.
    ///
    /// Results of scheduled requests are available via [`Self::recv`] (order is preserved).
    pub(crate) async fn make_request(
        &mut self,
        request: ClientGetAddrInfoRequest,
    ) -> AgentResult<()> {
        let (response_tx, response_rx) = oneshot::channel();

        let command = DnsCommand {
            request,
            response_tx,
        };
        if self.request_tx.send(command).await.is_err() {
            return Err(self.task_status.unwrap_err().await);
        }

        self.responses.push_back(response_rx);

        Ok(())
    }

    /// Returns the result of the oldest outstanding DNS request issued with this struct (see
    /// [`Self::make_request`]).
    ///
    /// If there is no outstanding DNS request, never returns.
    #[tracing::instrument(level = Level::TRACE, skip(self), ret, err)]
    pub(crate) async fn recv(&mut self) -> AgentResult<GetAddrInfoResponse> {
        let Some(response) = self.responses.next().await else {
            return future::pending().await;
        };

        let response = response
            .map_err(|_| AgentError::DnsTaskPanic)?
            .map_err(|error| ResponseError::DnsLookup(DnsLookupError { kind: error.into() }));

        Ok(GetAddrInfoResponse(response))
    }
}

#[derive(Error, Debug)]
enum InternalLookupError {
    #[error("failed to read configuration from /etc: {0}")]
    ReadConfigurationError(#[from] io::Error),
    #[error("resolve error: {0}")]
    ResolveError(#[from] ResolveError),
}

impl From<InternalLookupError> for ResolveErrorKindInternal {
    fn from(value: InternalLookupError) -> Self {
        match value {
            InternalLookupError::ReadConfigurationError(error) => error.kind().convert(),
            InternalLookupError::ResolveError(error) => error.kind().convert(),
        }
    }
}

trait ProtocolConversion<T> {
    fn convert(self) -> T;
}

impl ProtocolConversion<ResolveErrorKindInternal> for io::ErrorKind {
    fn convert(self) -> ResolveErrorKindInternal {
        match self {
            Self::TimedOut => ResolveErrorKindInternal::Timeout,
            Self::NotFound => ResolveErrorKindInternal::NotFound,
            Self::PermissionDenied => ResolveErrorKindInternal::PermissionDenied,
            other => ResolveErrorKindInternal::Message(format!("io error: {other}")),
        }
    }
}

impl ProtocolConversion<ResolveErrorKindInternal> for &ResolveErrorKind {
    fn convert(self) -> ResolveErrorKindInternal {
        match self {
            ResolveErrorKind::Message(message) => {
                ResolveErrorKindInternal::Message(message.to_string())
            }
            ResolveErrorKind::Msg(message) => ResolveErrorKindInternal::Message(message.clone()),
            ResolveErrorKind::NoConnections => ResolveErrorKindInternal::NoConnections,
            ResolveErrorKind::NoRecordsFound { response_code, .. } => {
                ResolveErrorKindInternal::NoRecordsFound((*response_code).into())
            }
            ResolveErrorKind::Proto(proto_error) => match proto_error.kind.as_ref() {
                ProtoErrorKind::Timeout => ResolveErrorKindInternal::Timeout,
                ProtoErrorKind::Io(e) => e.kind().convert(),
                error => ResolveErrorKindInternal::Message(format!("proto error: {error}")),
            },
            ResolveErrorKind::Timeout => ResolveErrorKindInternal::Timeout,
            ResolveErrorKind::Io(e) => e.kind().convert(),
            _ => {
                warn!(
                    error_kind = ?self,
                    "Detected an unhandled ResolveErrorKind, this is a bug"
                );
                ResolveErrorKindInternal::Unknown
            }
        }
    }
}

impl ProtocolConversion<DnsLookup> for LookupIp {
    fn convert(self) -> DnsLookup {
        let lookup_records = self
            .as_lookup()
            .records()
            .iter()
            .filter_map(|record| {
                let ip = record.data()?.ip_addr()?;
                Some(LookupRecord {
                    name: record.name().to_string(),
                    ip,
                })
            })
            .collect::<Vec<_>>();

        DnsLookup(lookup_records)
    }
}

impl ProtocolConversion<LookupIpStrategy> for AddressFamily {
    fn convert(self) -> LookupIpStrategy {
        match self {
            AddressFamily::Ipv4Only => LookupIpStrategy::Ipv4Only,
            AddressFamily::Ipv6Only => LookupIpStrategy::Ipv6Only,
            AddressFamily::Both => LookupIpStrategy::Ipv4AndIpv6,
            AddressFamily::Any => LookupIpStrategy::Ipv4thenIpv6,

            AddressFamily::UnknownAddressFamilyFromNewerClient => {
                tracing::error!("Unknown address family in addrinfo request. Using IPv4 and IPv6.");
                // If the agent gets some new, unknown variant of family address, it's the
                // client's fault, so the agent queries both IPv4 and IPv6 and if that's not
                // good enough for the client, the client can error out.
                LookupIpStrategy::Ipv4AndIpv6
            }
        }
    }
}
