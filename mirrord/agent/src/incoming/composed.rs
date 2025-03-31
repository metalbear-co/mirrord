use std::{fmt, ops::Not};

use futures::{stream::FuturesUnordered, StreamExt};

use super::{PortRedirector, Redirected};

/// An implementation of a [`PortRedirector`] that uses multiple inner redirectors.
#[derive(Debug)]
pub struct ComposedRedirector<R> {
    redirectors: Vec<R>,
}

impl<R> ComposedRedirector<R> {
    /// Created a new [`ComposedRedirector`] that will use the provided inner redirectors.
    ///
    /// # Panic
    ///
    /// Panics if the given redirectors vector is empty.
    pub fn new(redirectors: Vec<R>) -> Self {
        assert!(
            redirectors.is_empty().not(),
            "cannot create a ComposedRedirector with no inner redirectors"
        );

        Self { redirectors }
    }
}

impl<R> PortRedirector for ComposedRedirector<R>
where
    R: PortRedirector + fmt::Debug,
    R::Error: fmt::Display,
{
    type Error = R::Error;

    /// Called in order on all inner redirectors.
    ///
    /// Stops at the first error.
    async fn add_redirection(&mut self, from_port: u16) -> Result<(), Self::Error> {
        for redirector in &mut self.redirectors {
            redirector.add_redirection(from_port).await?;
        }

        Ok(())
    }

    /// Called in order on all inner redirectors.
    ///
    /// Stops at the first error.
    async fn remove_redirection(&mut self, from_port: u16) -> Result<(), Self::Error> {
        for redirector in &mut self.redirectors {
            redirector.remove_redirection(from_port).await?;
        }

        Ok(())
    }

    /// Called in order on all inner redirectors.
    ///
    /// Returns the last encountered error.
    /// All errors are logged.
    async fn cleanup(&mut self) -> Result<(), Self::Error> {
        let mut result = Ok(());

        for redirector in &mut self.redirectors {
            if let Err(error) = redirector.cleanup().await {
                tracing::error!(
                    %error,
                    ?redirector,
                    "Failed to do cleanup on a port redirector",
                );

                result = Err(error);
            }
        }

        result
    }

    /// Concurrently polls all inner redirectors for the next connection.
    ///
    /// Returns the first result.
    async fn next_connection(&mut self) -> Result<Redirected, Self::Error> {
        self.redirectors
            .iter_mut()
            .map(R::next_connection)
            .collect::<FuturesUnordered<_>>()
            .next()
            .await
            .expect("ComposedRedirector cannot be created with no inner redirector")
    }
}
