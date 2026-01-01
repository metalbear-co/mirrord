use std::collections::HashMap;

use bincode::{Decode, Encode};

#[derive(Default, Encode, Decode, Debug, PartialEq, Eq, Clone, Hash)]
pub enum WhichMetric {
    ClientCount,
    DnsRequestCount,
    OpenFdCount,
    MirrorPortSubscription,
    MirrorConnectionSubscription,
    StealFilteredPortSubscription,
    StealUnfilteredPortSubscription,
    RedirectedConnections,
    RedirectedRequests,
    TcpOutgoingConnection,
    UdpOutgoingConnection,
    BypassedHttpRequests,
    All,
    #[default]
    Unknown,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct MetricsRequest {
    pub agent_id: u128,
    pub metric: WhichMetric,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct MetricsResponse {
    pub agent_id: u128,
    pub metrics: HashMap<WhichMetric, i64>,
}
