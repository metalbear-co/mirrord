use std::ops::Not;

use futures::StreamExt;

use super::{IncomingStream, IncomingStreamItem};

pub mod dummy_redirector;
pub mod http;
mod task_basic;
pub mod tcp;

impl IncomingStream {
    pub async fn assert_data(&mut self, data: &[u8]) {
        assert!(data.len() > 0, "should not be called with empty data");
        let mut buffer = Vec::with_capacity(data.len());

        loop {
            match self.next().await.unwrap() {
                IncomingStreamItem::Data(new_data) => {
                    buffer.extend_from_slice(&new_data);
                    if &buffer == data {
                        break;
                    } else if data.starts_with(&buffer).not() {
                        panic!("unexpected data {buffer:?}, expected {data:?}");
                    }
                }
                other => panic!(
                    "unexpected message: {other:?}, data so far: `{}`",
                    String::from_utf8_lossy(&buffer)
                ),
            }
        }
    }
}
