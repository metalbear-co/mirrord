use std::{future::Future, panic::AssertUnwindSafe};

use futures::FutureExt;
use thiserror::Error;
use tokio::{
    sync::mpsc::{self, Receiver, Sender},
    task::JoinSet,
};

/// Errors that can occurr when the [`System`] runs [`Component`]s.
#[derive(Error, Debug)]
pub enum ComponentError<Id, E> {
    #[error("component {0} failed: {1}")]
    Error(Id, E),
    #[error("component {0} panicked")]
    Panic(Id),
}

/// Trait for internal proxy components.
pub trait Component: 'static + Send {
    type Message: 'static + Send;
    type Error: 'static + Send;
    type Id: 'static + Send;

    /// Returns the identifier of this component.
    fn id(&self) -> Self::Id;

    /// Handles messages sent to this component.
    fn run(
        self,
        message_rx: Receiver<Self::Message>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

/// A [`Result`] from a [`Component`] run.
pub type ComponentResult<T, E> = Result<T, ComponentError<T, E>>;

/// A simple component manager.
pub struct System<Id, E> {
    /// Registered components.
    components: JoinSet<ComponentResult<Id, E>>,
}

impl<Id, E> Default for System<Id, E> {
    fn default() -> Self {
        Self {
            components: Default::default(),
        }
    }
}

impl<Id, E> System<Id, E>
where
    Id: 'static + Send,
    E: 'static + Send,
{
    /// Capacity of [`mpsc::channel`]s used to communicate with components.
    const CHANNEL_CAPACITY: usize = 512;

    async fn component_task<COMP>(
        message_rx: Receiver<COMP::Message>,
        component: COMP,
    ) -> ComponentResult<Id, E>
    where
        COMP: Component,
        COMP::Id: Into<Id>,
        COMP::Error: Into<E>,
    {
        let id = component.id();
        let res = AssertUnwindSafe(component.run(message_rx))
            .catch_unwind()
            .await;

        match res {
            Err(..) => Err(ComponentError::Panic(id.into())),
            Ok(Err(e)) => Err(ComponentError::Error(id.into(), e.into())),
            Ok(Ok(())) => Ok(id.into()),
        }
    }

    /// Registers a new component in this struct. When this component shuts down, the result will be
    /// available via [`System::next`]. Returns [`ComponentRef`] to this component.
    pub fn register<COMP>(&mut self, component: COMP) -> ComponentRef<COMP>
    where
        COMP: Component,
        COMP::Id: Into<Id>,
        COMP::Error: Into<E>,
    {
        let (message_tx, message_rx) = mpsc::channel::<COMP::Message>(Self::CHANNEL_CAPACITY);

        self.components
            .spawn(Self::component_task(message_rx, component));

        ComponentRef { message_tx }
    }

    /// Waits until one of the attached components shuts down and returns its result.
    /// Returns [`None`] if there is no running component.
    pub async fn next(&mut self) -> Option<ComponentResult<Id, E>> {
        self.components
            .join_next()
            .await?
            .expect("catch_unwind should catch component panic")
            .into()
    }
}

pub struct ComponentRef<COMP: Component> {
    message_tx: Sender<COMP::Message>,
}

impl<COMP: Component> Clone for ComponentRef<COMP> {
    fn clone(&self) -> Self {
        Self {
            message_tx: self.message_tx.clone(),
        }
    }
}

impl<COMP: Component> ComponentRef<COMP> {
    #[clippy::must_use]
    pub async fn send<MSG: Into<COMP::Message>>(&self, message: MSG) {
        let _ = self.message_tx.send(message.into()).await;
    }
}

#[cfg(test)]
mod test {
    use tokio::sync::mpsc::{self, error::SendError, Receiver, Sender};

    use super::{Component, System};

    #[tokio::test]
    async fn system() {
        impl Component for Sender<usize> {
            type Message = usize;
            type Id = &'static str;
            type Error = SendError<usize>;

            fn id(&self) -> Self::Id {
                "sender"
            }

            async fn run(self, mut message_rx: Receiver<usize>) -> Result<(), Self::Error> {
                while let Some(message) = message_rx.recv().await {
                    self.send(message).await?;
                }

                Ok(())
            }
        }

        let (tx, mut rx) = mpsc::channel::<usize>(1);

        let mut system: System<&'static str, SendError<usize>> = Default::default();

        let component_ref = system.register(tx);
        component_ref.send(1_usize).await;

        let result = rx.recv().await.unwrap();

        assert_eq!(result, 1);

        std::mem::drop(component_ref);

        let id = system.next().await.unwrap().unwrap();
        assert_eq!(id, "sender");

        assert!(system.next().await.is_none());
    }
}
