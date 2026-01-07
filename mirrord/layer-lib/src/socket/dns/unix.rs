/// Handles the remote communication part of [`getaddrinfo`], call this if you want to resolve a DNS
/// through the agent, but don't need to deal with all the [`libc::getaddrinfo`] stuff.
///
/// # Note
///
/// This function updates the mapping in [`REMOTE_DNS_REVERSE_MAPPING`].
/// TODO(Daniel): Use dns.rs version for both unix and windows and remove this implementation.
#[cfg(unix)]
#[mirrord_layer_macro::instrument(level = Level::TRACE, ret, err)]
pub(super) fn remote_getaddrinfo(
    node: String,
    service_port: u16,
    flags: c_int,
    family: c_int,
    socktype: c_int,
    protocol: c_int,
) -> HookResult<Vec<(String, IpAddr)>> {
    let family = match family {
        libc::AF_INET => AddressFamily::Ipv4Only,
        libc::AF_INET6 => AddressFamily::Ipv6Only,
        _ => AddressFamily::Both,
    };
    let socktype = match socktype {
        libc::SOCK_STREAM => SockType::Stream,
        libc::SOCK_DGRAM => SockType::Dgram,
        _ => SockType::Any,
    };
    let addr_info_list = common::make_proxy_request_with_response(GetAddrInfoRequestV2 {
        node,
        service_port,
        flags,
        family,
        socktype,
        protocol,
    })?
    .0?;

    let mut remote_dns_reverse_mapping = REMOTE_DNS_REVERSE_MAPPING.lock()?;
    addr_info_list.iter().for_each(|lookup| {
        remote_dns_reverse_mapping.insert(lookup.ip, lookup.name.clone());
    });

    Ok(addr_info_list
        .into_iter()
        .map(|LookupRecord { name, ip }| (name, ip))
        .collect())
}

/// Retrieves the result of calling `getaddrinfo` from a remote host (resolves remote DNS),
/// converting the result into a `Box` allocated raw pointer of `libc::addrinfo` (which is basically
/// a linked list of such type).
///
/// Even though individual parts of the received list may contain an error, this function will
/// still work fine, as it filters out such errors and returns a null pointer in this case.
///
/// # Protocol
///
/// `-layer` sends a request to `-agent` asking for the `-agent`'s list of `addrinfo`s (remote call
/// for the equivalent of this function).
#[cfg(unix)]
#[mirrord_layer_macro::instrument(level = "trace", ret)]
pub(super) fn getaddrinfo(
    rawish_node: Option<&CStr>,
    rawish_service: Option<&CStr>,
    raw_hints: Option<&libc::addrinfo>,
) -> Detour<*mut libc::addrinfo> {
    let node: String = rawish_node
        .bypass(Bypass::NullNode)?
        .to_str()
        .map_err(|fail| {
            warn!(
                "Failed converting `rawish_node` from `CStr` with {:#?}",
                fail
            );

            Bypass::CStrConversion
        })?
        .into();

    // Convert `service` to port
    let service = rawish_service
        .map(CStr::to_str)
        .transpose()
        .map_err(|fail| {
            warn!(
                "Failed converting `raw_service` from `CStr` with {:#?}",
                fail
            );

            Bypass::CStrConversion
        })?
        // TODO: according to the man page, service could also be a service name, it doesn't have to
        //   be a port number.
        .and_then(|service| service.parse::<u16>().ok())
        .unwrap_or(0);

    let setup = crate::setup();
    setup.dns_selector().check_query(&node, service)?;
    let ipv6_enabled = setup.layer_config().feature.network.ipv6;

    let raw_hints = raw_hints
        .cloned()
        .unwrap_or_else(|| unsafe { mem::zeroed() });

    let libc::addrinfo {
        ai_family,
        ai_socktype,
        ai_protocol,
        ai_flags,
        ..
    } = raw_hints;

    // Some apps (gRPC on Python) use `::` to listen on all interfaces, and usually that just means
    // resolve on unspecified. So we just return that in IPv4, if IPv6 support is disabled.
    let resolved_addr = if ipv6_enabled.not() && (node == "::") {
        // name is "" because that's what happens in real flow.
        vec![("".to_string(), IpAddr::V4(Ipv4Addr::UNSPECIFIED))]
    } else {
        remote_getaddrinfo(
            node.clone(),
            service,
            ai_flags,
            ai_family,
            ai_socktype,
            ai_protocol,
        )?
    };

    let mut managed_addr_info = MANAGED_ADDRINFO.lock()?;
    // Only care about: `ai_family`, `ai_socktype`, `ai_protocol`.
    let result = resolved_addr
        .into_iter()
        .map(|(name, address)| {
            let rawish_sock_addr = SockAddr::from(SocketAddr::new(address, service));
            let ai_addrlen = rawish_sock_addr.len();
            let ai_family = rawish_sock_addr.family() as _;

            // Must outlive this function, as it is stored as a pointer in `libc::addrinfo`.
            let ai_addr = Box::into_raw(Box::new(unsafe { *rawish_sock_addr.as_ptr() }));
            let ai_canonname = CString::new(name).unwrap().into_raw();

            libc::addrinfo {
                ai_flags: 0,
                ai_family,
                ai_socktype,
                // TODO(alex): Don't just reuse whatever the user passed to us.
                ai_protocol,
                ai_addrlen,
                ai_addr,
                ai_canonname,
                ai_next: ptr::null_mut(),
            }
        })
        .rev()
        .map(Box::new)
        .map(Box::into_raw)
        .inspect(|&raw| {
            managed_addr_info.insert(raw as usize);
        })
        .reduce(|current, previous| {
            // Safety: These pointers were just allocated using `Box::new`, so they should be
            // fine regarding memory layout, and are not dangling.
            unsafe { (*previous).ai_next = current };
            previous
        })
        .ok_or(HookError::DNSNoName)?;

    trace!("getaddrinfo -> result {:#?}", result);

    Detour::Success(result)
}
