use core::ops::Deref;
use std::{collections::HashMap, net::IpAddr};

use bincode::{Decode, Encode};
use serde::{Deserialize, Serialize};
use trust_dns_resolver::{
    lookup_ip::LookupIp,
    proto::rr::{resource::RecordParts, Record},
    Name,
};

use crate::RemoteResult;

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct LookupRecord {
    pub name: String,
    pub ip: IpAddr,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct DnsLookup(Vec<LookupRecord>);

impl From<LookupIp> for DnsLookup {
    fn from(lookup_ip: LookupIp) -> Self {
        let lookup_records = lookup_ip
            .as_lookup()
            .records()
            .into_iter()
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

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct GetAddrInfoResponse(pub RemoteResult<DnsLookup>);

/// Triggered by the `mirrord-layer` hook of `getaddrinfo_detour`.
///
/// Even though all parameters are optional, at least one of `node` or `service` must be `Some`,
/// otherwise this will result in a `ResponseError`.
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct GetAddrInfoRequest {
    pub node: Option<String>,
}
