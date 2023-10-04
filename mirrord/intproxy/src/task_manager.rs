use std::{
    collections::{hash_map::Entry, HashMap},
    future::Future,
    hash::Hash,
    panic::AssertUnwindSafe,
};

use futures::FutureExt;
use thiserror::Error;
use tokio::sync::mpsc::{self, Receiver, Sender};

#[derive(Error, Debug)]
pub enum TaskManagerError<T: Task> {
    TaskAlreadyExists(T::Id),
    TaskDoesNotExist(T::Id),
}

pub struct MessageBus<T: Task> {
    id: T::Id,
    tx: Sender<TaskMessageOut<T>>,
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

    pub async fn recv(&mut self) -> T::MessageIn {
        self.rx.recv().await.expect("task channel closed")
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

pub enum TaskError<T: Task> {
    Error(T::Error),
    Panic,
}

pub enum TaskMessageOut<T: Task> {
    Raw {
        id: T::Id,
        inner: T::MessageOut,
    },
    Result {
        id: T::Id,
        inner: Result<(), TaskError<T>>,
    },
}

pub struct TaskManager<T: Task> {
    channel_capacity: usize,
    main_rx: Receiver<TaskMessageOut<T>>,
    main_tx: Sender<TaskMessageOut<T>>,
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

    pub fn spawn(&mut self, task: T) -> Result<(), TaskManagerError<T>> {
        let (tx, rx) = mpsc::channel(self.channel_capacity);

        match self.task_txs.entry(task.id()) {
            Entry::Occupied(..) => return Err(TaskManagerError::TaskAlreadyExists(task.id())),
            Entry::Vacant(e) => {
                e.insert(tx);
            }
        }

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
                Ok(Err(e)) => Err(TaskError::Error(e)),
                Ok(Ok(())) => Ok(()),
            };

            let _ = main_tx_clone
                .send(TaskMessageOut::Result { id, inner: result })
                .await;
        });

        Ok(())
    }

    pub async fn send(&self, task_id: T::Id, msg: T::MessageIn) -> Result<(), TaskManagerError<T>> {
        self.task_txs
            .get(&task_id)
            .ok_or(TaskManagerError::TaskDoesNotExist(task_id))?
            .send(msg)
            .await
            .map_err(|_| TaskManagerError::TaskDoesNotExist(task_id))
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
