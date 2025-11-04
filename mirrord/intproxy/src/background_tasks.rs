//! Logic for managing background tasks in the internal proxy.
//!
//! The proxy utilizes multiple background tasks to split the code into more self-contained parts.
//! Structs in this module aim to ease managing their state.
//!
//! Each background task implements the [`BackgroundTask`] trait, which specifies its properties and
//! allows for managing groups of related tasks with one [`BackgroundTasks`] instance.

use std::{collections::HashMap, fmt, future::Future, hash::Hash, ops::ControlFlow};

use mirrord_protocol::ClientMessage;
use mirrord_protocol_io::{Client, TxHandle};
use thiserror::Error;
use tokio::{
    sync::mpsc::{self, Receiver, Sender},
    task::JoinHandle,
};
use tokio_stream::{StreamExt, StreamMap, StreamNotifyClose, wrappers::ReceiverStream};
use tokio_util::sync::{CancellationToken, DropGuard};

pub type MessageBus<T> =
    MessageBusInner<<T as BackgroundTask>::MessageIn, <T as BackgroundTask>::MessageOut>;

/// A struct that is meant to be the only way the [`BackgroundTask`]s can communicate with their
/// parents. It allows the tasks to send and receive messages.
pub struct MessageBusInner<MessageIn, MessageOut> {
    tx: Sender<MessageOut>,
    rx: Receiver<MessageIn>,
    agent_tx: TxHandle<Client>,
    token: CancellationToken,
}

impl<MessageIn, MessageOut> MessageBusInner<MessageIn, MessageOut> {
    /// Attempts to send a message to this task's parent.
    pub async fn send<M: Into<MessageOut>>(&self, msg: M) {
        let _ = self.tx.send(msg.into()).await;
    }

    /// Receives a message from this task's parent.
    ///
    /// [`None`] means that the channel is closed and there will be no more messages.
    pub async fn recv(&mut self) -> Option<MessageIn> {
        tokio::select! {
            _ = self.tx.closed() => None,
            msg = self.rx.recv() => msg,
        }
    }

    /// Sends a message to the agent connection task.
    pub async fn send_agent(&self, msg: ClientMessage) {
        self.agent_tx.send(msg).await
    }

    /// Creates a clone of the agent tx handle
    pub fn clone_agent_tx(&self) -> TxHandle<Client> {
        self.agent_tx.clone()
    }

    /// Returns a [`CancellationToken`] that will be cancelled once this message bus is closed.
    ///
    /// Enables waiting for message bus close without consuming messages buffered in the channel.
    pub(crate) fn closed_token(&self) -> &CancellationToken {
        &self.token
    }
}

/// Common trait for all background tasks in the internal proxy.
pub trait BackgroundTask: Sized {
    /// Type of errors that can occur during the execution.
    type Error;
    /// Type of messages consumed by the task.
    type MessageIn;
    /// Type of messages produced by the task.
    type MessageOut;

    /// Runs this tasks.
    /// The task can use the provided [`MessageBus`] for communication.
    /// When the [`MessageBus`] has no more messages to be consumed, the task should exit without
    /// errors.
    fn run(
        &mut self,
        message_bus: &mut MessageBus<Self>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

pub trait RestartableBackgroundTask: BackgroundTask {
    fn restart(
        &mut self,
        run_error: Self::Error,
        message_bus: &mut MessageBus<Self>,
    ) -> impl Future<Output = ControlFlow<Self::Error>> + Send;
}

/// Small wrapper for `RestartableBackgroundTask` that wraps in reimplemets `BackgroundTask` with
/// logic of calling `restart` when an error is returned from `run` future.
///
/// This is the created and used in `BackgroundTasks::register_restartable`
pub struct RestartableBackgroundTaskWrapper<T: RestartableBackgroundTask> {
    task: T,
}

impl<T> BackgroundTask for RestartableBackgroundTaskWrapper<T>
where
    T: RestartableBackgroundTask + Send,
    T::MessageIn: Send,
    T::MessageOut: Send,
    T::Error: Send,
{
    type Error = T::Error;
    type MessageIn = T::MessageIn;
    type MessageOut = T::MessageOut;

    async fn run(&mut self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        let RestartableBackgroundTaskWrapper { task } = self;

        match task.run(message_bus).await {
            Err(run_error) => {
                let mut run_error = Some(run_error);

                loop {
                    match task
                        .restart(
                            run_error.take().expect("should contain an error"),
                            message_bus,
                        )
                        .await
                    {
                        ControlFlow::Break(err) => return Err(err),
                        ControlFlow::Continue(()) => match task.run(message_bus).await {
                            Err(err) => {
                                run_error = Some(err);
                            }
                            _ => {
                                return Ok(());
                            }
                        },
                    }
                }
            }
            Ok(()) => Ok(()),
        }
    }
}

/// A struct for managing groups of related [`BackgroundTasks`].
/// Tasks managed with a single instance of this struct must produce messages of the same type
/// `MOut` and return errors convertible to `Err`.
pub struct BackgroundTasks<Id, MOut, Err> {
    suspended_streams: HashMap<Id, StreamNotifyClose<ReceiverStream<MOut>>>,
    streams: StreamMap<Id, StreamNotifyClose<ReceiverStream<MOut>>>,
    handles: HashMap<Id, JoinHandle<Result<(), Err>>>,
    agent_tx: TxHandle<Client>,
}

impl<Id, MOut, Err> BackgroundTasks<Id, MOut, Err> {
    pub fn new(agent_tx: TxHandle<Client>) -> Self {
        Self {
            suspended_streams: Default::default(),
            streams: Default::default(),
            handles: Default::default(),
            agent_tx,
        }
    }

    pub fn set_agent_tx(&mut self, agent_tx: TxHandle<Client>) {
        self.agent_tx = agent_tx;
    }
}

impl<Id, MOut, Err> BackgroundTasks<Id, MOut, Err>
where
    Id: fmt::Debug + Hash + PartialEq + Eq + Clone + Unpin,
    Err: 'static + Send,
    MOut: 'static + Send + Unpin,
{
    /// Registers a new background task in this struct. Returns a [`TaskSender`] that can be used to
    /// send messages to the task. Dropping this sender will close the channel of messages
    /// consumed by the task (see [`MessageBus`]). This should trigger task exit.
    ///
    /// # Arguments
    ///
    /// * `task` - the [`BackgroundTask`] to be spawned and managed.
    /// * `id` - unique identifier of the task.
    /// * `channel_size` - size of [`mpsc`] channels used to communicate with the task.
    ///
    /// # Panics
    ///
    /// This method panics when attempting to register a task with a duplicate id.
    pub fn register<T>(&mut self, mut task: T, id: Id, channel_size: usize) -> TaskSender<T>
    where
        T: 'static + BackgroundTask<MessageOut = MOut> + Send,
        Err: From<T::Error>,
        T::MessageIn: Send,
    {
        if self.streams.contains_key(&id) {
            panic!("duplicate task id {id:?}");
        }

        let (in_msg_tx, in_msg_rx) = mpsc::channel(channel_size);
        let (out_msg_tx, out_msg_rx) = mpsc::channel(channel_size);

        self.streams.insert(
            id.clone(),
            StreamNotifyClose::new(ReceiverStream::new(out_msg_rx)),
        );

        let token = CancellationToken::new();

        let mut message_bus = MessageBus::<T> {
            tx: out_msg_tx,
            rx: in_msg_rx,
            token: token.clone(),
            agent_tx: self.agent_tx.another(),
        };

        self.handles.insert(
            id.clone(),
            tokio::spawn(async move { task.run(&mut message_bus).await.map_err(Into::into) }),
        );

        TaskSender {
            tx: in_msg_tx,
            _drop_guard: token.drop_guard(),
        }
    }

    pub fn register_restartable<T>(
        &mut self,
        task: T,
        id: Id,
        channel_size: usize,
    ) -> TaskSender<RestartableBackgroundTaskWrapper<T>>
    where
        T: 'static + RestartableBackgroundTask<MessageOut = MOut> + Send,
        Err: From<T::Error>,
        T::MessageIn: Send,
        T::Error: Send,
    {
        self.register(RestartableBackgroundTaskWrapper { task }, id, channel_size)
    }

    pub fn clear(&mut self) {
        self.handles = Default::default();
        self.streams = Default::default();
        self.suspended_streams = Default::default();
    }

    /// Returns the next update from one of registered tasks.
    pub async fn next(&mut self) -> Option<(Id, TaskUpdate<MOut, Err>)> {
        let (id, msg) = self.streams.next().await?;

        let msg = match msg {
            Some(msg) => (id, TaskUpdate::Message(msg)),
            None => {
                let res = self
                    .handles
                    .remove(&id)
                    .expect("task handles and streams are out of sync")
                    .await;
                match res {
                    Err(error) => {
                        tracing::error!(?error, "task panicked");
                        (id, TaskUpdate::Finished(Err(TaskError::Panic)))
                    }
                    Ok(res) => (id, TaskUpdate::Finished(res.map_err(TaskError::Error))),
                }
            }
        };

        Some(msg)
    }

    /// Waits for all registered tasks to finish and returns their results.
    /// This method does not signalize the tasks to finish. Instead, one should drop all
    /// [`TaskSender`]s first.
    pub async fn results(self) -> Vec<(Id, Result<(), TaskError<Err>>)> {
        std::mem::drop(self.streams);

        let mut results = Vec::with_capacity(self.handles.len());
        for (id, handle) in self.handles {
            let result = match handle.await {
                Err(error) => {
                    tracing::error!(?error, "task panicked");
                    Err(TaskError::Panic)
                }
                Ok(res) => res.map_err(TaskError::Error),
            };
            results.push((id, result));
        }

        results
    }

    /// Temporarily suspends messages from the registered task with the given id.
    ///
    /// [Self::next] will not return messages from this task,
    /// until you resume the messages with [`Self::resume_messages`].
    ///
    /// If the messages from this task are already suspended or the task is not registered, does
    /// nothing.
    pub fn suspend_messages(&mut self, id: Id) {
        if let Some(stream) = self.streams.remove(&id) {
            self.suspended_streams.insert(id, stream);
        }
    }

    /// Resumes messages from the registered task with the given id.
    ///
    /// If the messages from this task are not suspended or the task is not registered, does
    /// nothing.
    pub fn resume_messages(&mut self, id: Id) {
        if let Some(stream) = self.suspended_streams.remove(&id) {
            self.streams.insert(id, stream);
        }
    }
}

/// An error that can occur when executing a [`BackgroundTask`].
#[derive(Debug, Error)]
#[cfg_attr(test, derive(PartialEq, Eq))]
pub enum TaskError<Err> {
    /// An internal task error.
    #[error(transparent)]
    Error(Err),
    /// A panic.
    #[error("task panicked")]
    Panic,
}

/// An update received from a [`BackgroundTask`] registered in the [`BackgroundTasks`] struct.
#[derive(Debug)]
pub enum TaskUpdate<MOut, Err> {
    /// The task produced a message.
    Message(MOut),
    /// The task finished and was deregistered.
    Finished(Result<(), TaskError<Err>>),
}

#[cfg(test)]
impl<MOut, Err: fmt::Debug> TaskUpdate<MOut, Err> {
    pub fn unwrap_message(self) -> MOut {
        match self {
            Self::Message(mout) => mout,
            Self::Finished(res) => panic!("expected a message, got task result: {res:?}"),
        }
    }
}

/// A struct that can be used to send messages to a [`BackgroundTask`] registered in the
/// [`BackgroundTasks`] struct.
///
/// Dropping this sender will close the channel of messages consumed by
/// the task (see [`MessageBus`]). This should trigger task exit.
pub struct TaskSender<T: BackgroundTask> {
    tx: Sender<T::MessageIn>,
    _drop_guard: DropGuard,
}

impl<T: BackgroundTask> TaskSender<T> {
    /// Attempt to send a message to the task.
    pub async fn send<M: Into<T::MessageIn>>(&self, msg: M) {
        let _ = self.tx.send(msg.into()).await;
    }
}
