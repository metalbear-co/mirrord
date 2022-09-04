use mirrord_protocol::GetAddrInfoRequest;

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
    .map_err(|fail| ResponseError::from(std::io::Error::from(fail)))
    // Stable rust equivalent to `Result::flatten`.
    .and_then(std::convert::identity)
}

pub async fn dns_worker(
    rx: Receiver<GetAddrInfoRequest>,
    tx: Sender<RemoteResult<Vec<AddrInfoInternal>>>,
    pid: Option<u64>,
) -> Result<()> {
    if let Some(pid) = pid {
        let namespace = PathBuf::from("/proc")
            .join(PathBuf::from(pid.to_string()))
            .join(PathBuf::from("ns/net"));

        set_namespace(namespace)?;
    };

    loop {
        let request = rx.recv().await?;
        let result = get_addr_info(request);
        tx.send(result).await?;
    }

    Ok(())
}
