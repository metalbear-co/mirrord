//! Home for most of the HTTP stealer implementation / modules.
//!
//! # [`HttpV`]
//!
//! Helper trait to deal with [`hyper`] differences betwen HTTP/1 and HTTP/2 types.
//!
//! # [`HttpVersion`]
//!
//! # [`HttpFilterManager`]
//!
//! Holds the filters for a port we're stealing HTTP traffic on.

use self::reversible_stream::ReversibleStream;
use crate::http::HttpVersion;

mod filter;
mod reversible_stream;

pub use filter::HttpFilter;

/// Handy alias due to [`ReversibleStream`] being generic, avoiding value mismatches.
pub(crate) type DefaultReversibleStream = ReversibleStream<{ HttpVersion::MINIMAL_HEADER_SIZE }>;
