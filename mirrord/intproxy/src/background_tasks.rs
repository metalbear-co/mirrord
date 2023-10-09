//! Logic for managing background tasks in the internal proxy.
//! The proxy utilizes multiple background tasks to split the code into more self-contained parts.
//! Structs in this module aim to ease managing their state.
//!
//! Each background task implement the [`BackgroundTask`] trait, which specifies its properties and
//! allows for managing groups of related tasks with one [`BackgroundTasks`] instance.

use std::{collections::HashMap, fmt, future::Future, hash::Hash};

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
    pub async fn send(&self, msg: T::MessageOut) {
        let _ = self.tx.send(msg).await;
    }

    /// Receives a message from this task's parent.
    /// [`None`] means that the channel is closed and there will be no more messages.
    pub async fn recv(&mut self) -> Option<T::MessageIn> {
        tokio::select! {
            _ = self.tx.closed() => None,
            msg = self.rx.recv() => msg,
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
        self,
        message_bus: &mut MessageBus<Self>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
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
    pub fn register<T>(&mut self, task: T, id: Id, channel_size: usize) -> TaskSender<T::MessageIn>
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

    /// Returns whether this struct has any registered tasks.
    pub fn is_empty(&self) -> bool {
        self.streams.is_empty()
    }

    /// Returns the next update from one of registered tasks.
    pub async fn next(&mut self) -> (Id, TaskUpdate<MOut, Err>) {
        let (id, msg) = self.streams.next().await.unwrap();

        match msg {
            Some(msg) => (id, TaskUpdate::Message(msg)),
            None => {
                let res = self
                    .handles
                    .remove(&id)
                    .expect("task handles and streams are out of sync")
                    .await;
                match res {
                    Err(..) => (id, TaskUpdate::Finished(Err(TaskError::Panic))),
                    Ok(res) => (id, TaskUpdate::Finished(res.map_err(TaskError::Error))),
                }
            }
        }
    }

    /// Waits for all registered tasks to finish and returns their results.
    /// This method does not signalize the tasks to finish. Instead, one should drop all
    /// [`TaskSender`]s first.
    pub async fn results(self) -> Vec<(Id, Result<(), TaskError<Err>>)> {
        std::mem::drop(self.streams);

        let mut results = Vec::with_capacity(self.handles.len());
        for (id, handle) in self.handles {
            let result = match handle.await {
                Err(..) => Err(TaskError::Panic),
                Ok(res) => res.map_err(TaskError::Error),
            };
            results.push((id, result));
        }

        results
    }
}

/// An error that can occur when executing a [`BackgroundTask`].
#[derive(Debug)]
pub enum TaskError<Err> {
    /// An internal task error.
    Error(Err),
    /// A panic.
    Panic,
}

/// An update received from a [`BackgroundTask`] registered in the [`BackgroundTasks`] struct.
pub enum TaskUpdate<MOut, Err> {
    /// The task produced a message.
    Message(MOut),
    /// The task finished and was deregistered.
    Finished(Result<(), TaskError<Err>>),
}

/// A struct that can be used to send messages to a [`BackgroundTask`] registered in the
/// [`BackgroundTasks`] struct. Dropping this sender will close the channel of messages consumed by
/// the task (see [`MessageBus`]). This should trigger task exit.
pub struct TaskSender<M>(Sender<M>);

impl<M> TaskSender<M> {
    /// Attempt to send a message to the task.
    pub async fn send(&self, message: M) {
        let _ = self.0.send(message).await;
    }
}
