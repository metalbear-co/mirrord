extern crate alloc;
use core::ops::Deref;
use std::net::IpAddr;

use bincode::{Decode, Encode};
use trust_dns_resolver::{lookup_ip::LookupIp, proto::rr::resource::RecordParts};

use crate::RemoteResult;

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct LookupRecord {
    pub name: String,
    pub ip: IpAddr,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct DnsLookup(pub Vec<LookupRecord>);

impl From<LookupIp> for DnsLookup {
    fn from(lookup_ip: LookupIp) -> Self {
        let lookup_records = lookup_ip
            .as_lookup()
            .records()
            .iter()
            .cloned()
            .filter_map(|record| {
                let RecordParts {
                    name_labels, rdata, ..
                } = record.into_parts();

                rdata
                    .and_then(|rdata| rdata.to_ip_addr())
                    .map(|ip| LookupRecord {
                        name: name_labels.to_string(),
                        ip,
                    })
            })
            .collect::<Vec<_>>();

        Self(lookup_records)
    }
}

impl Deref for DnsLookup {
    type Target = Vec<LookupRecord>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl IntoIterator for DnsLookup {
    type Item = LookupRecord;

    type IntoIter = alloc::vec::IntoIter<Self::Item>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct GetAddrInfoResponse(pub RemoteResult<DnsLookup>);

impl Deref for GetAddrInfoResponse {
    type Target = RemoteResult<DnsLookup>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

/// Triggered by the `mirrord-layer` hook of `getaddrinfo_detour`.
///
/// Even though all parameters are optional, at least one of `node` or `service` must be `Some`,
/// otherwise this will result in a `ResponseError`.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct GetAddrInfoRequest {
    pub node: Option<String>,
}
