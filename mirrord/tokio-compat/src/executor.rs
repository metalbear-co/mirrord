//! Tokio executor integration for hyper.

use std::future::Future;

use hyper::rt::Executor;

/// Relies on [`tokio::spawn`] to execute futures.
#[derive(Default, Debug, Clone)]
pub struct TokioExecutor;

impl<Fut> Executor<Fut> for TokioExecutor
where
    Fut: Future + Send + 'static,
    Fut::Output: Send + 'static,
{
    fn execute(&self, fut: Fut) {
        tokio::spawn(fut);
    }
}
