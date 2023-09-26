// use std::{collections::VecDeque, marker::PhantomData};

// use crate::{
//     error::{IntProxyError, Result},
//     layer_conn::LayerSender,
//     protocol::{LayerRequest, LocalMessage},
// };

// pub struct RequestQueue<T> {
//     response_sender: LayerSender,
//     queued_ids: VecDeque<u64>,
//     _response_type: PhantomData<T>,
// }

// impl<T: LayerRequest> RequestQueue<T> {
//     pub fn new(response_sender: LayerSender) -> Self {
//         Self {
//             response_sender,
//             queued_ids: Default::default(),
//             _response_type: Default::default(),
//         }
//     }

//     pub fn queue(&mut self, request_id: u64) {
//         self.queued_ids.push_back(request_id)
//     }

//     pub async fn resolve_one(&mut self, response: T::Response) -> Result<()> {
//         let message_id = self
//             .queued_ids
//             .pop_front()
//             .ok_or(IntProxyError::RequestQueueEmpty)?;

//         self.response_sender
//             .send(LocalMessage {
//                 message_id,
//                 inner: T::wrap_response(response),
//             })
//             .await?;

//         Ok(())
//     }
// }
