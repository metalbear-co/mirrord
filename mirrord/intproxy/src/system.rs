use std::{fmt, future::Future, panic::AssertUnwindSafe, time::Duration};

use futures::FutureExt;
use thiserror::Error;
use tokio::{
    sync::mpsc::{self, Receiver, Sender, WeakSender},
    task::JoinSet, time,
};
use tokio_util::sync::{CancellationToken, DropGuard};

use crate::error::IntProxyError;

/// Errors that can occurr when the [`System`] runs [`Component`]s.
#[derive(Error, Debug)]
pub enum ComponentError<ID> {
    #[error("component {0} failed: {1}")]
    Error(ID, Box<IntProxyError>),
    #[error("component {0} panicked")]
    Panic(ID),
}

impl<ID: fmt::Display> ComponentError<ID> {
    /// Replaces [`Component::Id`] with a [`String`] in this struct.
    pub fn only_name(self) -> ComponentError<String> {
        match self {
            Self::Error(id, error) => ComponentError::Error(id.to_string(), error),
            Self::Panic(id) => ComponentError::Panic(id.to_string()),
        }
    }
}

/// Trait for internal proxy components.
pub trait Component: 'static + Send {
    type Message: 'static + Send;
    type Id: fmt::Display;

    /// Returns the identifier of this component.
    fn id(&self) -> Self::Id;

    /// Handles a message to this component.
    fn handle(
        &mut self,
        message: Self::Message,
    ) -> impl Future<Output = Result<(), IntProxyError>> + Send;
}

/// A [`Result`] from a [`Component`] run.
pub type ComponentResult<T> = core::result::Result<T, ComponentError<T>>;

/// A simple component manager.
#[derive(Default)]
pub struct System {
    /// Registered components.
    components: JoinSet<ComponentResult<String>>,
}

impl System {
    /// Capacity of [`mpsc::channel`]s used to communicate with components.
    const CHANNEL_CAPACITY: usize = 512;

    async fn component_task<COMP: Component>(
        mut message_rx: Receiver<COMP::Message>,
        mut component: COMP,
    ) -> ComponentResult<COMP::Id> {
        while let Some(message) = message_rx.recv().await {
            AssertUnwindSafe(component.handle(message))
                .catch_unwind()
                .await
                .map_err(|_| ComponentError::Panic(component.id()))?
                .map_err(|inner| ComponentError::Error(component.id(), inner.into()))?;
        }

        Ok(component.id())
    }

    /// Registers a new component in this struct. The component will run as long as it does not
    /// return an error nor panic and at least one of its [`ComponentRef`]s lives.
    ///
    /// When this component shuts down, the result will be available via [`System::next`].
    pub fn register<COMP: Component>(&mut self, component: COMP) -> ComponentRef<COMP> {
        let (message_tx, message_rx) = mpsc::channel::<COMP::Message>(Self::CHANNEL_CAPACITY);

        self.components.spawn(async move {
            Self::component_task(message_rx, component)
                .await
                .map(|id| id.to_string())
                .map_err(ComponentError::only_name)
        });

        ComponentRef { message_tx }
    }

    /// Same as [`System::register`], but allows to create component using a [`ComponentRef`] to
    /// itself.
    pub fn register_self_referencing<COMP, F>(&mut self, create_component: F) -> ComponentRef<COMP>
    where
        COMP: Component,
        F: FnOnce(ComponentRef<COMP>) -> COMP,
    {
        let (message_tx, message_rx) = mpsc::channel::<COMP::Message>(Self::CHANNEL_CAPACITY);
        let component_ref = ComponentRef { message_tx };
        let component = create_component(component_ref.clone());

        self.components.spawn(async move {
            Self::component_task(message_rx, component)
                .await
                .map(|id| id.to_string())
                .map_err(ComponentError::only_name)
        });

        component_ref
    }

    /// Waits until one of the attached components shuts down and returns its result.
    /// Returns [`None`] if there is no running component.
    pub async fn next(&mut self) -> Option<ComponentResult<String>> {
        let res = self
            .components
            .join_next()
            .await?
            .expect("catch_unwind should catch component panic");
        Some(res)
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

    /// Runs a new related component. The component will run as long as it does not
    /// return an error nor panic and at least one of its [`ComponentRef`]s lives.
    ///
    /// When this component shuts down, the result will be sent through this struct.
    #[clippy::must_use]
    pub fn spawn_related<COMP2>(self, component: COMP2) -> ComponentRef<COMP2>
    where
        COMP: Component,
        COMP::Message: From<ComponentResult<COMP2::Id>>,
        COMP2: Component,
        COMP2::Id: Send,
    {
        let (message_tx, message_rx) = mpsc::channel::<COMP2::Message>(System::CHANNEL_CAPACITY);

        tokio::spawn(async move {
            let res = System::component_task(message_rx, component).await;
            self.send(res).await;
        });

        ComponentRef { message_tx }
    }

    pub fn delay<MSG>(self, delay: Duration, message: MSG) -> DropGuard
    where
        MSG: Into<COMP::Message> + Send,
    {
        let token = CancellationToken::new();
        let guard = token.clone().drop_guard();
        let weak = self.downgrade();

        tokio::spawn(async move {
            time::sleep(delay).await;

            if token.is_cancelled() {
                return;
            }

            if let Some(strong) = weak.upgrade() {
                strong.send(message).await;
            }
        });

        guard
    }

    pub fn downgrade(self) -> WeakComponentRef<COMP> {
        WeakComponentRef {
            message_tx: self.message_tx.downgrade(),
        }
    }
}

pub struct WeakComponentRef<COMP: Component> {
    message_tx: WeakSender<COMP::Message>,
}

impl<COMP: Component> WeakComponentRef<COMP> {
    pub fn upgrade(&self) -> Option<ComponentRef<COMP>> {
        let message_tx = self.message_tx.upgrade()?;
        Some(ComponentRef { message_tx })
    }
}

pub trait Producer: 'static + Send {
    type Id: fmt::Display;
    type Message: 'static + Send;

    fn id(&self) -> Self::Id;

    fn next(&mut self)
        -> impl Future<Output = Option<Result<Self::Message, IntProxyError>>> + Send;
}

impl<COMP: Component> WeakComponentRef<COMP> {
    pub fn spawn_producer<PROD>(self, producer: PROD) -> DropGuard
    where
        PROD: Producer,
        PROD::Id: Send,
        COMP::Message: From<PROD::Message>,
        COMP::Message: From<ComponentResult<PROD::Id>>,
    {
        let cancellation_token = CancellationToken::new();

        tokio::spawn(async move {
            let res = loop {
                tokio::select! {
                    _ = cancellation_token.cancelled() => break Ok(producer.id()),
                    msg = AssertUnwindSafe(producer.next()).catch_unwind() => match msg {
                        Err(_) => break Err(ComponentError::Panic(producer.id())),
                        Ok(Some(Ok(msg))) => {
                            let Some(strong) = self.upgrade() else { return };
                            strong.send(msg).await;
                        }
                        Ok(Some(Err(err))) => break Err(ComponentError::Error(producer.id(), err.into())),
                        Ok(None) => break Ok(producer.id()),
                    }
                }
            };

            if let Some(strong) = self.upgrade() {
                strong.send(res).await;
            }
        });

        cancellation_token.drop_guard()
    }
}

#[cfg(test)]
mod test {
    use tokio::sync::mpsc::{self, Sender};

    use super::{Component, System};
    use crate::error::IntProxyError;

    #[tokio::test]
    async fn system() {
        impl Component for Sender<usize> {
            type Message = usize;
            type Id = String;

            fn id(&self) -> Self::Id {
                "mpsc sender".into()
            }

            async fn handle(&mut self, message: usize) -> Result<(), IntProxyError> {
                self.send(message).await.unwrap();
                Ok(())
            }
        }

        let (tx, mut rx) = mpsc::channel::<usize>(1);

        let mut system = System::default();
        let handle = system.register(tx);
        handle.send(1_usize).await;

        let result = rx.recv().await.unwrap();

        assert_eq!(result, 1);
    }
}
