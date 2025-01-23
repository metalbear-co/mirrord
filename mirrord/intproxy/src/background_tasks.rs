//! Logic for managing background tasks in the internal proxy.
//!
//! The proxy utilizes multiple background tasks to split the code into more self-contained parts.
//! Structs in this module aim to ease managing their state.
//!
//! Each background task implements the [`BackgroundTask`] trait, which specifies its properties and
//! allows for managing groups of related tasks with one [`BackgroundTasks`] instance.

use std::{collections::HashMap, fmt, future::Future, hash::Hash, ops::ControlFlow};

use thiserror::Error;
use tokio::{
    sync::mpsc::{self, Receiver, Sender},
    task::JoinHandle,
};
use tokio_stream::{wrappers::ReceiverStream, StreamExt, StreamMap, StreamNotifyClose};

/// A struct that is meant to be the only way the [`BackgroundTask`]s can communicate with their
/// parents. It allows the tasks to send and receive messages.
pub struct MessageBus<T: BackgroundTask> {
    tx: Sender<T::MessageOut>,
    rx: Receiver<T::MessageIn>,
}

impl<T: BackgroundTask> MessageBus<T> {
    /// Attempts to send a message to this task's parent.
    pub async fn send<M: Into<T::MessageOut>>(&self, msg: M) {
        let _ = self.tx.send(msg.into()).await;
    }

    /// Receives a message from this task's parent.
    /// [`None`] means that the channel is closed and there will be no more messages.
    pub async fn recv(&mut self) -> Option<T::MessageIn> {
        tokio::select! {
            _ = self.tx.closed() => None,
            msg = self.rx.recv() => msg,
        }
    }

    pub fn cast<R>(&mut self) -> &mut MessageBus<R>
    where
        R: BackgroundTask<MessageIn = T::MessageIn, MessageOut = T::MessageOut>,
    {
        unsafe { &mut *(self as *mut MessageBus<T> as *mut MessageBus<R>) }
    }

    /// Returns a [`Closed`] instance for this [`MessageBus`].
    pub(crate) fn closed(&self) -> Closed<T> {
        Closed(self.tx.clone())
    }
}

/// A helper struct bound to some [`MessageBus`] instance.
///
/// Used in [`BackgroundTask`]s to `.await` on [`Future`]s without lingering after their
/// [`MessageBus`] is closed.
///
/// Its lifetime does not depend on the origin [`MessageBus`] and it does not hold any references
/// to it, so that you can use it **and** the [`MessageBus`] at the same time.
///
/// # Usage example
///
/// ```ignore
/// use std::convert::Infallible;
///
/// use mirrord_intproxy::background_tasks::{BackgroundTask, Closed, MessageBus};
///
/// struct ExampleTask;
///
/// impl ExampleTask {
///     /// Thanks to the usage of [`Closed`] in [`Self::run`],
///     /// this function can freely resolve [`Future`]s and use the [`MessageBus`].
///     /// When the [`MessageBus`] is closed, the whole task will exit.
///     ///
///     /// To achieve the same without [`Closed`], you'd need to wrap each
///     /// [`Future`] resolution with [`tokio::select`].
///     async fn do_work(&self, message_bus: &mut MessageBus<Self>) {}
/// }
///
/// impl BackgroundTask for ExampleTask {
///     type MessageIn = Infallible;
///     type MessageOut = Infallible;
///     type Error = Infallible;
///
///     async fn run(self, message_bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
///         let closed: Closed<Self> = message_bus.closed();
///         closed.cancel_on_close(self.do_work(message_bus)).await;
///         Ok(())
///     }
/// }
/// ```
pub(crate) struct Closed<T: BackgroundTask>(Sender<T::MessageOut>);

impl<T: BackgroundTask> Closed<T> {
    /// Resolves the given [`Future`], unless the origin [`MessageBus`] closes first.
    ///
    /// # Returns
    ///
    /// * [`Some`] holding the future output - if the future resolved first
    /// * [`None`] - if the [`MessageBus`] closed first
    pub(crate) async fn cancel_on_close<F: Future>(&self, future: F) -> Option<F::Output> {
        tokio::select! {
            _ = self.0.closed() => None,
            output = future => Some(output)
        }
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

    async fn run(&mut self, bus: &mut MessageBus<Self>) -> Result<(), Self::Error> {
        let RestartableBackgroundTaskWrapper { task } = self;
        let task_bus = bus.cast();

        match task.run(task_bus).await {
            Err(run_error) => {
                let mut run_error = Some(run_error);

                loop {
                    match task
                        .restart(run_error.take().expect("should contain an error"), task_bus)
                        .await
                    {
                        ControlFlow::Break(err) => return Err(err),
                        ControlFlow::Continue(()) => {
                            if let Err(err) = task.run(task_bus).await {
                                run_error = Some(err);
                            } else {
                                return Ok(());
                            }
                        }
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
    streams: StreamMap<Id, StreamNotifyClose<ReceiverStream<MOut>>>,
    handles: HashMap<Id, JoinHandle<Result<(), Err>>>,
}

impl<Id, MOut, Err> Default for BackgroundTasks<Id, MOut, Err> {
    fn default() -> Self {
        Self {
            streams: Default::default(),
            handles: Default::default(),
        }
    }
}

impl<Id, MOut, Err> BackgroundTasks<Id, MOut, Err>
where
    Id: fmt::Debug + Hash + PartialEq + Eq + Clone + Unpin,
    Err: 'static + Send,
    MOut: Send + Unpin,
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

        let mut message_bus = MessageBus {
            tx: out_msg_tx,
            rx: in_msg_rx,
        };

        self.handles.insert(
            id.clone(),
            tokio::spawn(async move { task.run(&mut message_bus).await.map_err(Into::into) }),
        );

        TaskSender(in_msg_tx)
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

    pub fn tasks_ids(&self) -> impl Iterator<Item = &Id> {
        self.handles.keys()
    }

    pub async fn kill_task(&mut self, id: Id) {
        self.streams.remove(&id);
        let Some(task) = self.handles.remove(&id) else {
            return;
        };

        task.abort();
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

/// A struct that can be used to send messages to a [`BackgroundTask`] registered
///
/// A struct that can be used to send messages to a [`BackgroundTask`] registered in the
/// [`BackgroundTasks`] struct. Dropping this sender will close the channel of messages consumed by
/// the task (see [`MessageBus`]). This should trigger task exit.
pub struct TaskSender<T: BackgroundTask>(Sender<T::MessageIn>);

impl<T: BackgroundTask> TaskSender<T> {
    /// Attempt to send a message to the task.
    pub async fn send<M: Into<T::MessageIn>>(&self, msg: M) {
        let _ = self.0.send(msg.into()).await;
    }
}
