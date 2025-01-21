use std::{future, path::PathBuf, time::Duration};

use futures::{stream::FuturesOrdered, StreamExt};
use hickory_resolver::{system_conf::parse_resolv_conf, Hosts, Resolver};
use mirrord_protocol::{
    dns::{DnsLookup, GetAddrInfoRequest, GetAddrInfoResponse},
    DnsLookupError, RemoteResult, ResolveErrorKindInternal, ResponseError,
};
use tokio::{
    fs,
    sync::{
        mpsc::{Receiver, Sender},
        oneshot,
    },
};
use tokio_util::sync::CancellationToken;
use tracing::Level;

use crate::{error::AgentResult, metrics::DNS_REQUEST_COUNT, watched_task::TaskStatus};

#[derive(Debug)]
pub(crate) struct DnsCommand {
    request: GetAddrInfoRequest,
    response_tx: oneshot::Sender<RemoteResult<DnsLookup>>,
}

/// Background task for resolving hostnames to IP addresses.
/// Should be run in the same network namespace as the agent's target.
pub(crate) struct DnsWorker {
    etc_path: PathBuf,
    request_rx: Receiver<DnsCommand>,
    attempts: usize,
    timeout: Duration,
}

impl DnsWorker {
    pub const TASK_NAME: &'static str = "DNS worker";

    /// Creates a new instance of this worker.
    /// To run this worker, call [`Self::run`].
    ///
    /// # Note
    ///
    /// `pid` is used to find the correct path of `etc` directory.
    pub(crate) fn new(pid: Option<u64>, request_rx: Receiver<DnsCommand>) -> Self {
        let etc_path = pid
            .map(|pid| {
                PathBuf::from("/proc")
                    .join(pid.to_string())
                    .join("root/etc")
            })
            .unwrap_or_else(|| PathBuf::from("/etc"));

        Self {
            etc_path,
            request_rx,
            timeout: std::env::var("MIRRORD_AGENT_DNS_TIMEOUT")
                .ok()
                .and_then(|timeout| timeout.parse().ok())
                .map(Duration::from_secs)
                .unwrap_or_else(|| Duration::from_secs(1)),
            attempts: std::env::var("MIRRORD_AGENT_DNS_ATTEMPTS")
                .ok()
                .and_then(|attempts| attempts.parse().ok())
                .unwrap_or(1),
        }
    }

    /// Reads `/etc/resolv.conf` and `/etc/hosts` files, then uses [`hickory_resolver::Resolver`] to
    /// resolve address of the given `host`.
    ///
    /// # TODO
    ///
    /// We could probably cache results here.
    /// We cannot cache the [`Resolver`] itself, becaues the configuration in `etc` may change.
    #[tracing::instrument(level = Level::TRACE, ret, err(level = Level::TRACE))]
    async fn do_lookup(
        etc_path: PathBuf,
        host: String,
        attempts: usize,
        timeout: Duration,
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
            options.server_ordering_strategy =
                hickory_resolver::config::ServerOrderingStrategy::UserProvidedOrder;
            options.timeout = timeout;
            options.attempts = attempts;
            options.ip_strategy = hickory_resolver::config::LookupIpStrategy::Ipv4Only;

            let mut resolver = Resolver::tokio(config, options);

            let mut hosts = Hosts::default();
            hosts.read_hosts_conf(hosts_conf.as_slice())?;
            resolver.set_hosts(Some(hosts));

            resolver
        };

        let lookup = resolver
            .inspect_err(|fail| tracing::error!(?fail, "Failed to build DNS resolver"))?
            .lookup_ip(host)
            .await
            .inspect(|lookup| tracing::trace!(?lookup, "Lookup finished"))?
            .into();

        Ok(lookup)
    }

    /// Handles the given [`DnsCommand`] in a separate [`tokio::task`].
    #[tracing::instrument(level = Level::TRACE, skip(self))]
    fn handle_message(&self, message: DnsCommand) {
        let etc_path = self.etc_path.clone();
        let timeout = self.timeout;
        let attempts = self.attempts;

        DNS_REQUEST_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        let lookup_future = async move {
            let result = Self::do_lookup(etc_path, message.request.node, attempts, timeout).await;

            if let Err(result) = message.response_tx.send(result) {
                tracing::error!(?result, "Failed to send query response");
            }
            DNS_REQUEST_COUNT.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        };

        tokio::spawn(lookup_future);
    }

    pub(crate) async fn run(mut self, cancellation_token: CancellationToken) -> AgentResult<()> {
        loop {
            tokio::select! {
                _ = cancellation_token.cancelled() => break Ok(()),

                message = self.request_rx.recv() => match message {
                    None => break Ok(()),
                    Some(message) => self.handle_message(message),
                },
            }
        }
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
    /// Results of scheduled requests are available via [`Self::recv`] (order is preserved).
    pub(crate) async fn make_request(&mut self, request: GetAddrInfoRequest) -> AgentResult<()> {
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
    #[tracing::instrument(level = Level::TRACE, skip(self), ret, err)]
    pub(crate) async fn recv(&mut self) -> AgentResult<GetAddrInfoResponse> {
        let Some(response) = self.responses.next().await else {
            return future::pending().await;
        };

        let response = response?.map_err(|fail| match fail {
            ResponseError::RemoteIO(remote_ioerror) => ResponseError::DnsLookup(DnsLookupError {
                kind: remote_ioerror.kind.into(),
            }),
            fail @ ResponseError::DnsLookup(_) => fail,
            _ => ResponseError::DnsLookup(DnsLookupError {
                kind: ResolveErrorKindInternal::Unknown,
            }),
        });

        Ok(GetAddrInfoResponse(response))
    }
}
