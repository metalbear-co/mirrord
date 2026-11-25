//! Watching a service for the tasks that match a developer's selector.
//!
//! An intproxy registration is paired with one agent and consumed by that pairing. `mirrord up`
//! instead wants every task that matches, including the ones that start later, so it keeps one
//! registration open as a [`Watch`] and receives a [`TaskMatch`] for each matching task and a
//! [`TaskEnded`] when one goes away. It then connects to each task with an ordinary
//! registration, pinned to the task's replica.

use futures::Stream;
use mirrord_sessions_manager_protocol::{
    AgentSelector, AssignmentSubscription, IntproxyIdentity, IntproxySubscriptionOptions,
    ServiceScope, SubscriptionOptions, TaskEnded, TaskMatch, Watch,
};

use crate::{
    client::validate_scope,
    control_plane::{
        ControlPlaneEvent, ControlPlaneEventStream,
        subscriber::{ControlPlaneSubscriber, ControlPlaneSubscription},
    },
    error::SessionsManagerClientError,
    transport::SessionsManagerTransport,
};

/// What a [`TaskWatcher`] reports about the tasks of its service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskEvent {
    /// A task matched the selector. The sessions-manager sends a task again when the watch
    /// registers again, so a receiver deduplicates by `(replica_id, agent_instance_id)`.
    Matched(TaskMatch),
    /// A matched task is gone.
    Ended(TaskEnded),
}

/// A watching registration: an intproxy subscription that is never paired with an agent.
struct WatchSubscription {
    subscription: AssignmentSubscription,
    /// The encoded [`IntproxySubscriptionOptions`], which hold the selector and the watch.
    options: String,
}

impl ControlPlaneSubscription for WatchSubscription {
    type Output = TaskEvent;

    fn name(&self) -> &'static str {
        "task watch"
    }

    async fn subscribe<T: SessionsManagerTransport>(
        &self,
        transport: &T,
        scope: &ServiceScope,
    ) -> Result<ControlPlaneEventStream, SessionsManagerClientError> {
        transport
            .subscribe_assignments(scope, &self.subscription, Some(&self.options))
            .await
    }

    fn extract(
        &self,
        event: ControlPlaneEvent,
    ) -> Result<Option<Self::Output>, SessionsManagerClientError> {
        match event {
            ControlPlaneEvent::Matched(task) => Ok(Some(TaskEvent::Matched(task))),
            ControlPlaneEvent::Ended(task) => Ok(Some(TaskEvent::Ended(task))),
            ControlPlaneEvent::Superseded => Err(SessionsManagerClientError::Superseded),
            // A watch is never given a room, nor is it an agent.
            ControlPlaneEvent::Assignment(_) | ControlPlaneEvent::Admission(_) => Ok(None),
        }
    }
}

/// Reports the tasks of a service that match a selector, for as long as it is polled.
///
/// The registration is opened again whenever its stream breaks, under the same identity, so the
/// sessions-manager keeps treating it as the same watch, and re-sends `matched` for what it holds
/// for the session. It ends only on an error that reconnecting can't fix, such as a refused
/// selector or a newer registration with the same identity.
pub struct TaskWatcher<T> {
    subscriber: ControlPlaneSubscriber<WatchSubscription, T>,
}

impl<T: SessionsManagerTransport> TaskWatcher<T> {
    /// Registers a watch for the tasks in `scope` that match `agent_selector`, and returns once
    /// the sessions-manager accepted it.
    ///
    /// `user_session_id` names the developer's session. The connections made to the matched
    /// tasks use it too. `replace` asks for the matched tasks to be replaced by the developer's
    /// local processes.
    pub async fn open(
        transport: T,
        scope: ServiceScope,
        user_session_id: String,
        agent_selector: AgentSelector,
        replace: bool,
    ) -> Result<Self, SessionsManagerClientError> {
        let options = IntproxySubscriptionOptions {
            agent_selector: Some(agent_selector),
            watch: Some(Watch { replace }),
        }
        .encode();
        let subscription = WatchSubscription {
            subscription: AssignmentSubscription::Intproxy {
                identity: IntproxyIdentity {
                    user_session_id,
                    intproxy_connection_id: uuid::Uuid::new_v4().to_string(),
                },
                agent_replica_filter: None,
            },
            options,
        };

        let mut subscriber =
            ControlPlaneSubscriber::new(transport, validate_scope(scope)?, subscription, false);
        subscriber.open(None).await?;

        Ok(Self { subscriber })
    }

    /// Waits for the next task event, reconnecting as long as the errors allow it.
    pub async fn next(&mut self) -> Result<TaskEvent, SessionsManagerClientError> {
        self.subscriber.next(None).await
    }

    /// The events of [`Self::next`] as a stream, which ends after its first error.
    pub fn into_stream(
        self,
    ) -> impl Stream<Item = Result<TaskEvent, SessionsManagerClientError>> + Send + 'static
    where
        T: Send + 'static,
    {
        futures::stream::unfold(Some(self), |watcher| async move {
            let mut watcher = watcher?;
            match watcher.next().await {
                Ok(event) => Some((Ok(event), Some(watcher))),
                Err(error) => Some((Err(error), None)),
            }
        })
    }
}
