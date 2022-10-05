use std::{
    fs::{self, File},
    path::{Path, PathBuf},
};

use mirrord_protocol::{
    dns::{DnsLookup, GetAddrInfoRequest, GetAddrInfoResponse},
    RemoteResult,
};
use tokio::sync::{mpsc::Receiver, oneshot::Sender};
use tracing::{error, trace};
use trust_dns_resolver::{system_conf::parse_resolv_conf, AsyncResolver, Hosts};

use crate::{error::AgentError, runtime::set_namespace};

#[derive(Debug)]
pub struct DnsRequest {
    request: GetAddrInfoRequest,
    tx: Sender<GetAddrInfoResponse>,
}

impl DnsRequest {
    pub fn new(request: GetAddrInfoRequest, tx: Sender<GetAddrInfoResponse>) -> Self {
        Self { request, tx }
    }
}

// TODO(alex): aviram's suggested caching the resolver, but this should not be done by having a
// single cached resolver, as we use system files that might change, thus invalidating our cache.
// The cache should be hash-based.
/// Uses `AsyncResolver:::lookup_ip` to resolve `host`.
///
/// `root_path` is used to read `/proc/{pid}/root` configuration files when creating a resolver.
#[tracing::instrument(level = "trace")]
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
pub async fn dns_worker(mut rx: Receiver<DnsRequest>, pid: Option<u64>) -> Result<(), AgentError> {
    if let Some(pid) = pid {
        let namespace = PathBuf::from("/proc")
            .join(PathBuf::from(pid.to_string()))
            .join(PathBuf::from("ns"));
        set_namespace(namespace.join(PathBuf::from("net")))?;
    }

    let root_path = pid
        .map(|pid| PathBuf::from("/proc").join(pid.to_string()).join("root"))
        .unwrap_or_else(|| PathBuf::from("/"));

    while let Some(DnsRequest { request, tx }) = rx.recv().await {
        trace!("dns_worker -> request {:#?}", request);

        let result = dns_lookup(root_path.as_path(), request.node.unwrap());
        if let Err(result) = tx.send(GetAddrInfoResponse(result.await)) {
            error!("couldn't send result to caller {result:?}");
        }
    }

    Ok(())
}
