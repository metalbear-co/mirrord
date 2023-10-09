//! Sub-proxies of the internal proxy. Each of these encapsulate logic for handling a group of
//! related requests and communicate only with the [`ProxySession`](crate::session::ProxySession).

pub mod incoming;
pub mod outgoing;
pub mod simple;
