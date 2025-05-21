//! Utils related to stealing with an HTTP filter.

mod response_fallback;

pub(crate) use response_fallback::{HttpResponseFallback, ReceiverStreamBody};
