use amq_protocol_types::FieldTable;
use serde::{Deserialize, Serialize};

use super::SplitQueueNameDetails;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")] // name_source -> nameSource in yaml.
pub struct RmqQueueDetails {
    #[serde(flatten)]
    pub name_details: SplitQueueNameDetails,

    /// RabbitMQ specific arguments that will be used during for the cosume call.
    #[serde(default)]
    pub arguments: FieldTable,
}


impl Eq for RmqQueueDetails {}
