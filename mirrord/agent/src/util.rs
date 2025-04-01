use std::{
    clone::Clone,
    collections::{hash_map::Entry, HashMap, HashSet},
    future::Future,
    hash::Hash,
    pin::Pin,
    task::{Context, Poll},
};

use futures::{future::BoxFuture, FutureExt};
use tokio::sync::mpsc;

pub mod path_resolver;
pub mod remote_runtime;

/// Struct that helps you manage topic -> subscribers
///
/// When a topic has no subscribers, it is removed.
#[derive(Debug, Default)]
pub struct Subscriptions<T, C> {
    inner: HashMap<T, HashSet<C>>,
}

/// Id of an agent's client. Each new client connection is assigned with a unique id.
pub type ClientId = u32;

impl<T, C> Subscriptions<T, C>
where
    T: Eq + Hash + Clone + Copy,
    C: Eq + Hash + Clone + Copy,
{
    /// Add a new subscription to a topic for a given client.
    /// Returns whether this resulted in adding a new topic to this mapping.
    pub fn subscribe(&mut self, client: C, topic: T) -> bool {
        match self.inner.entry(topic) {
            Entry::Occupied(mut e) => {
                e.get_mut().insert(client);
                false
            }
            Entry::Vacant(e) => {
                e.insert([client].into());
                true
            }
        }
    }

    /// Remove a subscription of given client from the topic.
    /// Topic is removed if no subscribers left.
    /// Return whether the topic was removed.
    pub fn unsubscribe(&mut self, client: C, topic: T) -> bool {
        match self.inner.entry(topic) {
            Entry::Occupied(mut e) => {
                e.get_mut().remove(&client);
                if e.get().is_empty() {
                    e.remove();
                    true
                } else {
                    false
                }
            }
            Entry::Vacant(..) => false,
        }
    }

    /// Get a vector of clients subscribed to a specific topic
    pub fn get_topic_subscribers(&self, topic: T) -> Option<&HashSet<C>> {
        self.inner.get(&topic)
    }

    /// Get subscribed topics
    pub fn get_subscribed_topics(&self) -> Vec<T> {
        self.inner.keys().cloned().collect()
    }

    /// Remove all subscriptions of a client.
    /// Topics are removed if no subscribers left.
    /// Returns whether any topic was removed.
    pub fn remove_client(&mut self, client: C) -> bool {
        let prev_length = self.inner.len();

        self.inner.retain(|_, client_set| {
            client_set.remove(&client);
            !client_set.is_empty()
        });

        self.inner.len() != prev_length
    }

    /// Removes a topic and all of it's clients
    #[allow(dead_code)] // we might want it later on
    pub fn remove_topic(&mut self, topic: T) {
        self.inner.remove(&topic);
    }
}

/// [`Future`] that resolves to [`ClientId`] when the client drops their [`mpsc::Receiver`].
pub(crate) struct ChannelClosedFuture(BoxFuture<'static, ClientId>);

impl ChannelClosedFuture {
    pub(crate) fn new<T: 'static + Send>(tx: mpsc::Sender<T>, client_id: ClientId) -> Self {
        let future = async move {
            tx.closed().await;
            client_id
        }
        .boxed();

        Self(future)
    }
}

impl Future for ChannelClosedFuture {
    type Output = ClientId;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.get_mut().0.as_mut().poll(cx)
    }
}

#[cfg(test)]
mod subscription_tests {
    use std::hash::Hash;

    use mirrord_protocol::Port;

    use super::Subscriptions;

    impl<C, T> Subscriptions<T, C>
    where
        C: Hash + Eq,
        T: Copy,
    {
        /// Get topics subscribed by a client
        fn get_client_topics(&self, client: C) -> Vec<T> {
            let mut result = Vec::new();
            for (topic, client_set) in self.inner.iter() {
                if client_set.contains(&client) {
                    result.push(*topic)
                }
            }
            result
        }
    }

    #[test]
    fn sanity() {
        let mut subscriptions: Subscriptions<Port, _> = Default::default();
        subscriptions.subscribe(3, 2);
        subscriptions.subscribe(3, 3);
        subscriptions.subscribe(3, 1);
        subscriptions.subscribe(1, 4);
        subscriptions.subscribe(2, 1);
        subscriptions.subscribe(2, 1);
        subscriptions.subscribe(3, 1);
        let mut subscribers = subscriptions
            .get_topic_subscribers(1)
            .into_iter()
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        subscribers.sort();
        assert_eq!(subscribers, vec![2, 3]);

        assert_eq!(
            subscriptions
                .get_topic_subscribers(4)
                .into_iter()
                .flatten()
                .copied()
                .collect::<Vec<_>>(),
            vec![1]
        );
        assert!(subscriptions.get_topic_subscribers(10).is_none());
        let mut topics = subscriptions.get_subscribed_topics();
        topics.sort();
        assert_eq!(topics, vec![1, 2, 3, 4]);
        topics = subscriptions.get_client_topics(3);
        topics.sort();
        assert_eq!(topics, vec![1, 2, 3]);
        subscriptions.unsubscribe(3, 1);
        assert_eq!(
            subscriptions
                .get_topic_subscribers(1)
                .into_iter()
                .flatten()
                .copied()
                .collect::<Vec<_>>(),
            vec![2]
        );
        subscriptions.unsubscribe(1, 4);
        assert!(subscriptions.get_topic_subscribers(4).is_none());
        subscriptions.remove_client(3);
        assert_eq!(subscriptions.get_client_topics(3), Vec::<u16>::new());
        subscriptions.remove_topic(1);
        assert_eq!(subscriptions.get_subscribed_topics(), Vec::<u16>::new());
    }
}

#[cfg(test)]
mod channel_closed_tests {
    use std::time::Duration;

    use futures::{stream::FuturesUnordered, FutureExt, StreamExt};
    use rstest::rstest;

    use super::*;

    /// Verifies that [`ChannelClosedFuture`] resolves when the related [`mpsc::Receiver`] is
    /// dropped.
    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn channel_closed_resolves() {
        let (tx, rx) = mpsc::channel::<()>(1);
        let future = ChannelClosedFuture::new(tx, 0);
        std::mem::drop(rx);
        assert_eq!(future.await, 0);
    }

    /// Verifies that [`ChannelClosedFuture`] works fine when used in [`FuturesUnordered`].
    ///
    /// The future used to hold the [`mpsc::Sender`] and call poll [`mpsc::Sender::closed`] in it's
    /// [`Future::poll`] implementation. This worked fine when the future was used in a simple way
    /// ([`channel_closed_resolves`] test was passing).
    ///
    /// However, [`FuturesUnordered::next`] was hanging forever due to [`mpsc::Sender::closed`]
    /// implementation details.
    ///
    /// New implementation of [`ChannelClosedFuture`] uses a [`BoxFuture`] internally, which works
    /// fine.
    #[rstest]
    #[timeout(Duration::from_secs(5))]
    #[tokio::test]
    async fn channel_closed_works_in_futures_unordered() {
        let mut unordered: FuturesUnordered<ChannelClosedFuture> = FuturesUnordered::new();

        let (tx, rx) = mpsc::channel::<()>(1);
        let future = ChannelClosedFuture::new(tx, 0);

        unordered.push(future);

        assert!(unordered.next().now_or_never().is_none());
        std::mem::drop(rx);
        assert_eq!(unordered.next().await.unwrap(), 0);
    }
}
