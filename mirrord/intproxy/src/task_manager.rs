use std::{collections::HashMap, future::Future, hash::Hash, panic::AssertUnwindSafe};

use futures::FutureExt;
use thiserror::Error;
use tokio::sync::mpsc::{self, Receiver, Sender};

pub struct MessageBus<T: Task> {
    id: T::Id,
    tx: Sender<TaskMessageOut<T::Id, T::MessageOut, T::Error>>,
    rx: Receiver<T::MessageIn>,
}

impl<T: Task> MessageBus<T> {
    pub async fn send(&self, msg: T::MessageOut) {
        self.tx
            .send(TaskMessageOut::Raw {
                id: self.id,
                inner: msg,
            })
            .await
            .expect("task manager main channel closed")
    }

    pub async fn recv(&mut self) -> Option<T::MessageIn> {
        self.rx.recv().await
    }
}

pub trait Task: 'static + Send + Sized {
    type Id: Send + Copy + PartialEq + Eq + Hash;
    type Error: Send;
    type MessageIn: Send;
    type MessageOut: Send;

    fn id(&self) -> Self::Id;

    fn run(
        self,
        messages: &mut MessageBus<Self>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send;
}

#[derive(Error, Debug)]
pub enum TaskError<E> {
    #[error("{0}")]
    Error(E),
    #[error("panic")]
    Panic,
}

pub enum TaskMessageOut<I, M, E> {
    Raw {
        id: I,
        inner: M,
    },
    Result {
        id: I,
        inner: Result<(), TaskError<E>>,
    },
}

pub struct TaskManager<T: Task> {
    channel_capacity: usize,
    main_rx: Receiver<TaskMessageOut<T::Id, T::MessageOut, T::Error>>,
    main_tx: Sender<TaskMessageOut<T::Id, T::MessageOut, T::Error>>,
    task_txs: HashMap<T::Id, Sender<T::MessageIn>>,
}

impl<T: Task> TaskManager<T> {
    pub fn new(channel_capacity: usize) -> Self {
        let (main_tx, main_rx) = mpsc::channel(channel_capacity);

        Self {
            channel_capacity,
            main_rx,
            main_tx,
            task_txs: Default::default(),
        }
    }

    pub fn spawn(&mut self, task: T) {
        let (tx, rx) = mpsc::channel(self.channel_capacity);
        self.task_txs.insert(task.id(), tx);

        let main_tx_clone = self.main_tx.clone();

        let mut message_bus = MessageBus {
            id: task.id(),
            tx: self.main_tx.clone(),
            rx,
        };

        tokio::spawn(async move {
            let id = task.id();
            let res = AssertUnwindSafe(task.run(&mut message_bus))
                .catch_unwind()
                .await;

            let result = match res {
                Err(..) => Err(TaskError::Panic),
                Ok(res) => res.map_err(TaskError::Error),
            };

            let _ = main_tx_clone
                .send(TaskMessageOut::Result { id, inner: result })
                .await;
        });
    }

    pub async fn send(&self, task_id: T::Id, msg: T::MessageIn) {
        let Some(tx) = self.task_txs.get(&task_id) else {
            return;
        };

        let _ = tx.send(msg).await;
    }

    pub async fn receive(&mut self) -> TaskMessageOut<T> {
        let res = self
            .main_rx
            .recv()
            .await
            .expect("task manager main channel closed");

        if let TaskMessageOut::Result { id, .. } = &res {
            self.task_txs.remove(id);
        }

        res
    }
}

pub struct ManagedTask<T: Task> {
    tx: Sender<T::MessageIn>,
    rx: Receiver<TaskMessageOut<T::Id, T::MessageOut, T::Error>>,
}

impl<T: Task> ManagedTask<T> {
    pub fn spawn(task: T, channel_capacity: usize) -> Self {
        let (tx_in, rx_in) = mpsc::channel(channel_capacity);
        let (tx_out, rx_out) = mpsc::channel(channel_capacity);

        let mut message_bus = MessageBus {
            id: task.id(),
            tx: tx_out.clone(),
            rx: rx_in,
        };

        tokio::spawn(async move {
            let id = task.id();
            let res = AssertUnwindSafe(task.run(&mut message_bus))
                .catch_unwind()
                .await;

            let result = match res {
                Err(..) => Err(TaskError::Panic),
                Ok(res) => res.map_err(TaskError::Error),
            };

            let _ = tx_out
                .send(TaskMessageOut::Result { id, inner: result })
                .await;
        });

        Self {
            tx: tx_in,
            rx: rx_out,
        }
    }

    pub async fn send(&self, msg: T::MessageIn) {
        let _ = self.tx.send(msg).await;
    }

    pub async fn receive(&mut self) -> TaskMessageOut<T> {
        self.rx.recv().await.expect("cannot receive, task finished")
    }
}
