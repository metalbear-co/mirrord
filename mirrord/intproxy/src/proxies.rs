//! Sub-proxies of the internal proxy. Each of these encapsulate logic for handling a group of
//! related requests and communicate only with the [`ProxySession`](crate::session::ProxySession).
//! Each of those is run by every [`ProxySession`](crate::session::ProxySession) as a [`BackgroundTask`](crate::background_tasks::BackgroundTask).

pub mod incoming;
pub mod outgoing;
pub mod simple;
