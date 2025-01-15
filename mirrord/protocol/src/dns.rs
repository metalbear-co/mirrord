extern crate alloc;
use core::ops::Deref;
use std::{net::IpAddr, sync::LazyLock};

use bincode::{Decode, Encode};
use hickory_resolver::{lookup_ip::LookupIp, proto::rr::resource::RecordParts};
use semver::VersionReq;

use crate::RemoteResult;

/// Minimal mirrord-protocol version that allows [`GetAddrInfoRequestV2`].
pub static ADDRINFO_V2_VERSION: LazyLock<VersionReq> =
    LazyLock::new(|| ">=1.14.0".parse().expect("Bad Identifier"));

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

                rdata.ip_addr().map(|ip| LookupRecord {
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
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct GetAddrInfoRequest {
    pub node: String,
}

/// For when the new request is not supported, and we have to fall back to the old version.
impl From<GetAddrInfoRequestV2> for GetAddrInfoRequest {
    fn from(value: GetAddrInfoRequestV2) -> Self {
        Self { node: value.node }
    }
}

/// Newer, advanced version of [`GetAddrInfoRequest`]
#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct GetAddrInfoRequestV2 {
    pub node: String,
    pub service_port: u16,
    // TODO: should I use c_int?
    pub flags: i32,
    pub family: i32,
    pub socktype: i32,
    pub protocol: i32,
}

impl From<GetAddrInfoRequest> for GetAddrInfoRequestV2 {
    fn from(value: GetAddrInfoRequest) -> Self {
        Self {
            node: value.node,
            service_port: 0,
            flags: 0,
            family: 0,
            socktype: 0,
            protocol: 0,
        }
    }
}
