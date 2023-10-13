//! Sub-proxies of the internal proxy. Each of these encapsulates logic for handling a group of
//! related requests and exchanges messages only with the [`IntProxy`](crate::IntProxy).

pub mod incoming;
pub mod outgoing;
pub mod simple;
