use bincode::{Decode, Encode};

#[derive(Default, Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub enum Metric {
    BypassedHttpRequests,
    All,
    #[default]
    Unknown,
}

#[derive(Encode, Decode, Debug, PartialEq, Eq, Clone)]
pub struct MetricsRequest {
    metric: Metric,
}
