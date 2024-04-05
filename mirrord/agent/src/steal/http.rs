//! Utils related to stealing with an HTTP filter.

use crate::http::HttpVersion;

mod filter;
mod reversible_stream;

pub use filter::HttpFilter;

pub(crate) use self::reversible_stream::ReversibleStream;

/// Handy alias due to [`ReversibleStream`] being generic, avoiding value mismatches.
pub(crate) type DefaultReversibleStream = ReversibleStream<{ HttpVersion::MINIMAL_HEADER_SIZE }>;
