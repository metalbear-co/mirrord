use std::path::PathBuf;

use mirrord_protocol::{
    AddrInfoHint, AddrInfoInternal, GetAddrInfoRequest, RemoteResult, ResponseError,
};
use tokio::sync::{mpsc::Receiver, oneshot::Sender};
use tracing::{error, trace};

use crate::{error::AgentError, runtime::set_namespace};

#[derive(Debug)]
pub struct DnsRequest {
    request: GetAddrInfoRequest,
    tx: Sender<RemoteResult<Vec<AddrInfoInternal>>>,
}

impl DnsRequest {
    pub fn new(
        request: GetAddrInfoRequest,
        tx: Sender<RemoteResult<Vec<AddrInfoInternal>>>,
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

/// Handles the `getaddrinfo` call from mirrord-layer.
fn get_addr_info(request: GetAddrInfoRequest) -> RemoteResult<Vec<AddrInfoInternal>> {
    trace!("get_addr_info -> request {:#?}", request);

    let GetAddrInfoRequest {
        node,
        service,
        hints,
    } = request;

    dns_lookup::getaddrinfo(
        node.as_deref(),
        service.as_deref(),
        hints.map(|h| h.into_lookup()),
    )
    .map(|addrinfo_iter| {
        addrinfo_iter
            .map(|result| {
                // Each element in the iterator is actually a `Result<AddrInfo, E>`, so
                // we have to `map` individually, then convert to one of our errors.
                result.map(Into::into).map_err(From::from)
            })
            // Now we can flatten and transpose the whole thing into this.
            .collect::<Result<Vec<AddrInfoInternal>, _>>()
    })
    // We can't use the generic `io::Error` since it doesn't return the correct error code to the
    // layer.
    .map_err(ResponseError::from)
    // Stable rust equivalent to `Result::flatten`.
    .and_then(std::convert::identity)
}

pub async fn dns_worker(mut rx: Receiver<DnsRequest>, pid: Option<u64>) -> Result<(), AgentError> {
    if let Some(pid) = pid {
        let namespace = PathBuf::from("/proc")
            .join(PathBuf::from(pid.to_string()))
            .join(PathBuf::from("ns/net"));

        set_namespace(namespace)?;
    };

    while let Some(DnsRequest { request, tx }) = rx.recv().await {
        let result = get_addr_info(request);
        if let Err(result) = tx.send(result) {
            error!("couldn't send result to caller {result:?}");
        }
    }

    Ok(())
}
