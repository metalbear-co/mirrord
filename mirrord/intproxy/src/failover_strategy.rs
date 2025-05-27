use crate::background_tasks::TaskError;
use crate::error::InternalProxyError;
use crate::main_tasks::FromLayer;
use crate::{
    background_tasks::{BackgroundTasks, TaskSender, TaskUpdate},
    error::{IntProxyError, ProxyManagedError},
    layer_conn::LayerConnection,
    layer_initializer::LayerInitializer,
    main_tasks::{MainTaskId, ProxyMessage},
    IntProxy,
};
use mirrord_intproxy_protocol::{LayerId, LayerToProxyMessage, LocalMessage, ProxyToLayerMessage};
use std::{collections::HashMap, time::Duration};
use tokio::time;

pub(super) struct FailoverStrategy {
    background_tasks: BackgroundTasks<MainTaskId, ProxyMessage, InternalProxyError>,
    layer_initializer: TaskSender<LayerInitializer>,
    layers: HashMap<LayerId, TaskSender<LayerConnection>>,
    pending_layers: Vec<LayerId>,
    any_connection_accepted: bool,
    fail_cause: ProxyManagedError,
}

impl FailoverStrategy {
    pub fn fail_cause(&self) -> &ProxyManagedError {
        &self.fail_cause
    }

    fn has_layer_connections(&self) -> bool {
        !self.layers.is_empty()
    }

    pub fn from_failed_proxy(failed_proxy: IntProxy, error: ProxyManagedError) -> Self {
        
        FailoverStrategy {
            background_tasks: failed_proxy.background_tasks,
            layer_initializer: failed_proxy.task_txs._layer_initializer,
            layers: failed_proxy.task_txs.layers,
            pending_layers: failed_proxy.pending_layers.into_iter().collect(),
            any_connection_accepted: failed_proxy.any_connection_accepted,
            fail_cause: error,
        }
    }

    pub async fn run(
        self,
        first_timeout: Duration,
        idle_timeout: Duration,
    ) -> Result<(), IntProxyError> {
        let mut failover = self;

        while let Some(layer_id) = failover.pending_layers.pop(){
            failover.update_layer_on_error(layer_id).await;
        }

        loop {
            tokio::select! {
                Some((task_id, task_update)) = failover.background_tasks.next() => {
                    tracing::trace!(
                        %task_id,
                        ?task_update,
                        "Received a task update",
                    );
                    failover.handle_task_update(task_id, task_update).await;
                }
                _ = time::sleep(first_timeout), if !failover.any_connection_accepted => {
                    Err(IntProxyError::ConnectionAcceptTimeout)?;
                },
                _ = time::sleep(idle_timeout), if failover.any_connection_accepted && !failover.has_layer_connections() => {
                    tracing::info!("Reached the idle timeout with no active layer connections");
                    break;
                },
            }
        }

        std::mem::drop(failover.layer_initializer);
        std::mem::drop(failover.layers);

        tracing::info!("Collecting background task results before exiting");
        let results = failover.background_tasks.results().await;

        for (task_id, result) in results {
            tracing::trace!(
                %task_id,
                ?result,
                "Collected a background task result",
            );
        }

        Ok(())
    }

    async fn handle_task_update(
        & mut self,
        task_id: MainTaskId,
        update: TaskUpdate<ProxyMessage, InternalProxyError>,
    ) {
        match (task_id, update) {
            (MainTaskId::LayerConnection(LayerId(id)), TaskUpdate::Finished(Ok(()))) => {
                tracing::trace!(layer_id = id, "Layer connection closed");
                self.layers.remove(&LayerId(id));
            }
            (task_id, TaskUpdate::Finished(res)) => match res {
                Ok(()) => {
                    tracing::error!(%task_id, "One of the main tasks finished unexpectedly");
                }
                Err(TaskError::Error(error)) => {
                    tracing::error!(%task_id, %error, "One of the main tasks failed");
                }
                Err(TaskError::Panic) => {
                    tracing::error!(%task_id, "One of the main tasks panicked");
                }
            },

            (_, TaskUpdate::Message(msg)) => self.handle(msg).await,
        }
    }
    
    async fn handle(&mut self, msg: ProxyMessage){
        match msg {
            ProxyMessage::NewLayer(new_layer) => {
                self.any_connection_accepted = true;
                let tx = self.background_tasks.register(
                    LayerConnection::new(new_layer.stream, new_layer.id),
                    MainTaskId::LayerConnection(new_layer.id),
                    IntProxy::CHANNEL_SIZE,
                );
                self.layers.insert(new_layer.id, tx);
            }
            ProxyMessage::FromLayer(FromLayer{ message: msg @ LayerToProxyMessage::Incoming(_) , ..})  => {
                tracing::info!("proxy in failover mode, ignored msg {:?}", msg);
            }
            ProxyMessage::FromLayer(FromLayer{ layer_id , ..}) => {
                self.update_layer_on_error(layer_id).await;
            }
            msg =>{
                tracing::info!("proxy in failover mode, ignored msg {:?}", msg);
            }
        }
    }
    
    async fn update_layer_on_error(&self, layer_id: LayerId) {
        if let Some(layer) = self.layers.get(&layer_id) {
            layer.send(LocalMessage{
                message_id: rand::random(),
                inner: ProxyToLayerMessage::ProxyFailed(self.fail_cause.to_string()),
            }).await;
        } else {
            tracing::warn!("Layer {:?} not found, but it was waiting for proxy to respond!", layer_id);
        }
    }
    
}
