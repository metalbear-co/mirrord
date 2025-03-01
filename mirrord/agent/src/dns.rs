use std::{future, path::PathBuf, sync::atomic::Ordering, time::Duration, usize};

use futures::{stream::FuturesOrdered, StreamExt};
use hickory_resolver::{
    config::{LookupIpStrategy, ServerOrderingStrategy},
    system_conf::parse_resolv_conf,
    Hosts, TokioAsyncResolver,
};
use mirrord_agent_env::envs;
use mirrord_protocol::{
    dns::{DnsLookup, GetAddrInfoRequest, GetAddrInfoRequestV2, GetAddrInfoResponse},
    DnsLookupError, RemoteResult, ResolveErrorKindInternal, ResponseError,
};
use tokio::{
    fs,
    sync::{
        mpsc::{Receiver, Sender},
        oneshot,
    },
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;
use tracing::Level;

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

#[derive(Debug)]
pub(crate) struct DnsCommand {
    request: ClientGetAddrInfoRequest,
    response_tx: oneshot::Sender<RemoteResult<DnsLookup>>,
}

/// Background task for resolving hostnames to IP addresses.
/// Should be run in the same network namespace as the agent's target.
pub(crate) struct DnsWorker {
    etc_path: PathBuf,
    request_rx: Receiver<DnsCommand>,
    attempts: Option<usize>,
    timeout: Option<Duration>,
    support_ipv6: bool,
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
    ) -> RemoteResult<DnsLookup> {
        // Prepares the `Resolver` after reading some `/etc` DNS files.
        //
        // We care about logging these errors, at an `error!` level.
        let resolver: Result<_, ResponseError> = try {
            let resolv_conf_path = etc_path.join("resolv.conf");
            let hosts_path = etc_path.join("hosts");

            let resolv_conf = fs::read(resolv_conf_path).await?;
            let hosts_conf = fs::read(hosts_path).await?;

            let (config, mut options) = parse_resolv_conf(resolv_conf)?;
            tracing::error!(?config, ?options, "Parsed resolv configuration");

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
                request
                    .family
                    .try_into()
                    .inspect_err(|e| {
                        tracing::error!(%e,
                        "Unknown address family in addrinfo request. Using IPv4 and IPv6.")
                    })
                    // If the agent gets some new, unknown variant of family address, it's the
                    // client's fault, so the agent queries both IPv4 and IPv6 and if that's not
                    // good enough for the client, the client can error out.
                    .unwrap_or(LookupIpStrategy::Ipv4AndIpv6)
            } else {
                tracing::debug!("IPv6 support disabled. Resolving IPv4 only.");
                LookupIpStrategy::Ipv4Only
            };

            tracing::error!(?config, ?options, "Updated resolv configuration");

            let mut resolver = TokioAsyncResolver::tokio(config, options);
            tracing::trace!(?resolver, "Build a DNS resolver");

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
            .into();

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
    responses: FuturesOrdered<oneshot::Receiver<RemoteResult<DnsLookup>>>,
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
            .map_err(|fail| match fail {
                ResponseError::RemoteIO(remote_io_error) => {
                    ResponseError::DnsLookup(DnsLookupError {
                        kind: remote_io_error.kind.into(),
                    })
                }
                fail @ ResponseError::DnsLookup(_) => fail,
                _ => ResponseError::DnsLookup(DnsLookupError {
                    kind: ResolveErrorKindInternal::Unknown,
                }),
            });

        Ok(GetAddrInfoResponse(response))
    }
}
