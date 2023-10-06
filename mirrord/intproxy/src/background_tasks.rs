use std::{collections::HashMap, future::Future, hash::Hash};

use tokio::{
    sync::mpsc::{self, Receiver, Sender},
    task::JoinHandle,
};
use tokio_stream::{wrappers::ReceiverStream, StreamExt, StreamMap, StreamNotifyClose};

pub struct MessageBus<T: BackgroundTask> {
    tx: Sender<T::MessageOut>,
    rx: Receiver<T::MessageIn>,
}

impl<T: BackgroundTask> MessageBus<T> {
    pub async fn send(&self, msg: T::MessageOut) {
        let _ = self.tx.send(msg).await;
    }

    pub async fn recv(&mut self) -> Option<T::MessageIn> {
        tokio::select! {
            _ = self.tx.closed() => None,
            msg = self.rx.recv() => msg,
        }
    }
}

pub trait BackgroundTask: Sized {
    type Error;
    type MessageIn;
    type MessageOut;

    fn run(
        self,
        message_bus: &mut MessageBus<Self>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

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
    Id: Hash + PartialEq + Eq + Clone + Unpin,
    Err: 'static + Send,
    MOut: Send + Unpin,
{
    pub fn register<T>(&mut self, task: T, id: Id, channel_size: usize) -> TaskSender<T>
    where
        T: 'static + BackgroundTask<MessageOut = MOut> + Send,
        Err: From<T::Error>,
        T::MessageIn: Send,
    {
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
            id,
            tokio::spawn(async move { task.run(&mut message_bus).await.map_err(Into::into) }),
        );

        TaskSender(in_msg_tx)
    }

    pub fn is_empty(&self) -> bool {
        self.streams.is_empty()
    }

    pub async fn next(&mut self) -> (Id, TaskUpdate<MOut, Err>) {
        let (id, msg) = self.streams.next().await.unwrap();

        match msg {
            Some(msg) => (id, TaskUpdate::Message(msg)),
            None => {
                let res = self.handles.remove(&id).unwrap().await;
                match res {
                    Err(..) => (id, TaskUpdate::Finished(Err(TaskError::Panic))),
                    Ok(res) => (id, TaskUpdate::Finished(res.map_err(TaskError::Error))),
                }
            }
        }
    }

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

#[derive(Debug)]
pub enum TaskError<Err> {
    Error(Err),
    Panic,
}

pub enum TaskUpdate<MOut, Err> {
    Message(MOut),
    Finished(Result<(), TaskError<Err>>),
}

pub struct TaskSender<T: BackgroundTask>(Sender<T::MessageIn>);

impl<T: BackgroundTask> TaskSender<T> {
    pub async fn send(&self, message: T::MessageIn) {
        let _ = self.0.send(message).await;
    }
}
