//! Utils related to stealing with an HTTP filter.

use crate::http::HttpVersion;

mod filter;
mod response_fallback;
mod reversible_stream;

pub(crate) use filter::HttpFilter;
pub(crate) use response_fallback::{HttpResponseFallback, ReceiverStreamBody};
pub(crate) use reversible_stream::ReversibleStream;

/// Handy alias due to [`ReversibleStream`] being generic, avoiding value mismatches.
pub(crate) type DefaultReversibleStream = ReversibleStream<{ HttpVersion::MINIMAL_HEADER_SIZE }>;
