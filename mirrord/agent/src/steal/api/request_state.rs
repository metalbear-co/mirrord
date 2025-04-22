use mirrord_protocol::{
    tcp::{HttpRequest, InternalHttpBody, InternalHttpBodyFrame},
    ConnectionId, RequestId,
};
use tokio::sync::mpsc;

use crate::incoming::ResponseProvider;

pub struct RequestState {
    connection_id: ConnectionId,
    data_tx: Option<mpsc::Sender<Vec<u8>>>,
    request_in_progress: Option<HttpRequest<InternalHttpBody>>,
    response_tx: Option<ResponseProvider>,
    response_frame_tx: Option<mpsc::Sender<InternalHttpBodyFrame>>,
}

impl RequestState {
    pub const REQUEST_ID: RequestId = 0;
}
