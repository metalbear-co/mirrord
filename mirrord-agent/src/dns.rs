use std::{fs::File, net::IpAddr, os::unix::prelude::*, path::PathBuf};

use mirrord_protocol::{
    AddrInfoHint, AddrInfoInternal, GetAddrInfoRequest, RemoteResult, ResponseError,
};
use tokio::sync::{mpsc::Receiver, oneshot::Sender};
use tracing::{debug, error, trace};
use trust_dns_resolver::{
    config::{ResolverConfig, ResolverOpts},
    proto::rr::RecordType,
    system_conf::parse_resolv_conf,
    AsyncResolver, Resolver, TokioAsyncResolver,
};

use crate::{error::AgentError, runtime::set_namespace};

#[derive(Debug)]
pub struct DnsRequest {
    request: GetAddrInfoRequest,
    // tx: Sender<RemoteResult<Vec<AddrInfoInternal>>>,
    tx: Sender<RemoteResult<Vec<IpAddr>>>,
}

impl DnsRequest {
    pub fn new(
        request: GetAddrInfoRequest,
        // tx: Sender<RemoteResult<Vec<AddrInfoInternal>>>,
        tx: Sender<RemoteResult<Vec<IpAddr>>>,
    ) -> Self {
        Self { request, tx }
    }
}

trait AddrInfoHintExt {
    fn into_lookup(self) -> dns_lookup::AddrInfoHints;
}

impl AddrInfoHintExt for AddrInfoHint {
    fn into_lookup(self) -> dns_lookup::AddrInfoHints {
        dns_lookup::AddrInfoHints {
            socktype: self.ai_socktype,
            protocol: self.ai_protocol,
            address: self.ai_family,
            flags: self.ai_flags,
        }
    }
}

use std::fs;

#[tracing::instrument(level = "debug")]
async fn dns_lookup(root_path: PathBuf, host: String) -> RemoteResult<Vec<IpAddr>> {
    let resolv_conf_path = root_path.join("etc").join("resolv.conf");
    debug!("dns_lookup -> resolv_conf_path {:#?}", resolv_conf_path);

    let resolv_conf = fs::read(resolv_conf_path)?;
    debug!("dns_lookup -> resolv_conf {:#?}", resolv_conf);

    let (config, options) = parse_resolv_conf(resolv_conf)?;
    // let resolver = AsyncResolver::new(config, options)?;
    let tokio_resolver = TokioAsyncResolver::tokio(config, options)?;

    let lookup = tokio_resolver.lookup_ip(host).await?;
    let addresses = lookup.into_iter().collect::<Vec<_>>();
    Ok(addresses)
}

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
        debug!("dns_worker -> request {:#?}", request);

        let result = dns_lookup(root_path.clone(), request.node.unwrap());
        if let Err(result) = tx.send(result.await) {
            error!("couldn't send result to caller {result:?}");
        }
    }

    Ok(())
}
