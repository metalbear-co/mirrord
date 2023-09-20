use std::{
    fs::{self, File},
    path::{Path, PathBuf},
};

use mirrord_protocol::{
    dns::{DnsLookup, GetAddrInfoRequest, GetAddrInfoResponse},
    DnsLookupError, RemoteResult, ResolveErrorKindInternal, ResponseError,
};
use tokio::sync::{
    mpsc::{self, Receiver, Sender},
    oneshot,
};
use tracing::{error, trace};
use trust_dns_resolver::{system_conf::parse_resolv_conf, AsyncResolver, Hosts};

use crate::{
    error::Result,
    util::run_thread_in_namespace,
    watched_task::{TaskStatus, WatchedTask},
};

#[derive(Debug)]
pub(crate) struct DnsRequest {
    request: GetAddrInfoRequest,
    tx: oneshot::Sender<GetAddrInfoResponse>,
}

// TODO(alex): aviram's suggested caching the resolver, but this should not be done by having a
// single cached resolver, as we use system files that might change, thus invalidating our cache.
// The cache should be hash-based.
/// Uses `AsyncResolver:::lookup_ip` to resolve `host`.
///
/// `root_path` is used to read `/proc/{pid}/root` configuration files when creating a resolver.
#[tracing::instrument(level = "trace", ret)]
async fn dns_lookup(root_path: &Path, host: String) -> RemoteResult<DnsLookup> {
    let resolv_conf_path = root_path.join("etc").join("resolv.conf");
    let hosts_path = root_path.join("etc").join("hosts");

    let resolv_conf = fs::read(resolv_conf_path)?;
    let hosts_file = File::open(hosts_path)?;

    let (config, options) = parse_resolv_conf(resolv_conf)?;
    let mut resolver = AsyncResolver::tokio(config, options)?;

    let hosts = Hosts::default().read_hosts_conf(hosts_file)?;
    resolver.set_hosts(Some(hosts));

    let lookup = resolver
        .lookup_ip(host)
        .await
        .inspect(|lookup| trace!("lookup {lookup:#?}"))?
        .into();

    Ok(lookup)
}

/// Task for the DNS resolving thread that runs in the `/proc/{pid}/ns` network namespace.
///
/// Reads a `DnsRequest` from `rx` and returns the resolved addresses (or `Error`) through
/// `DnsRequest::tx`.
pub(crate) async fn dns_worker(mut rx: Receiver<DnsRequest>, pid: Option<u64>) -> Result<()> {
    let root_path = pid
        .map(|pid| PathBuf::from("/proc").join(pid.to_string()).join("root"))
        .unwrap_or_else(|| PathBuf::from("/"));

    while let Some(DnsRequest { request, tx }) = rx.recv().await {
        trace!("dns_worker -> request {:#?}", request);

        let result = dns_lookup(root_path.as_path(), request.node)
            .await
            .map_err(|err| {
                error!("dns_lookup -> ResponseError:: {err:?}");
                match err {
                    ResponseError::DnsLookup(err) => ResponseError::DnsLookup(err),
                    _ => ResponseError::DnsLookup(DnsLookupError {
                        kind: ResolveErrorKindInternal::Unknown,
                    }),
                }
            });
        if let Err(result) = tx.send(GetAddrInfoResponse(result)) {
            error!("couldn't send result to caller {result:?}");
        }
    }

    Ok(())
}

#[derive(Clone)]
pub(crate) struct DnsApi {
    task_status: TaskStatus,
    sender: Sender<DnsRequest>,
}

impl DnsApi {
    const TASK_NAME: &'static str = "DNS worker";

    pub fn new(pid: Option<u64>, channel_size: usize) -> Self {
        let (sender, receiver) = mpsc::channel(channel_size);

        let watched_task = WatchedTask::new(Self::TASK_NAME, dns_worker(receiver, pid));
        let task_status = watched_task.status();
        run_thread_in_namespace(
            watched_task.start(),
            Self::TASK_NAME.to_string(),
            pid,
            "net",
        );

        Self {
            task_status,
            sender,
        }
    }

    #[tracing::instrument(level = "trace", ret, skip(self))]
    async fn try_make_request(&self, request: GetAddrInfoRequest) -> Result<GetAddrInfoResponse> {
        let (tx, rx) = oneshot::channel();
        let request = DnsRequest { request, tx };
        self.sender.send(request).await?;
        rx.await.map_err(Into::into)
    }

    #[tracing::instrument(level = "trace", ret, skip(self))]
    pub async fn make_request(
        &mut self,
        request: GetAddrInfoRequest,
    ) -> Result<GetAddrInfoResponse> {
        match self.try_make_request(request).await {
            Ok(res) => Ok(res),
            Err(_) => Err(self.task_status.unwrap_err().await),
        }
    }
}
