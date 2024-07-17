use std::{
    clone::Clone,
    collections::{hash_map::Entry, HashMap, HashSet, VecDeque},
    future::Future,
    hash::Hash,
    thread::JoinHandle,
};

use num_traits::{CheckedAdd, Num};
use tracing::error;

use crate::{
    error::AgentError,
    namespace::{set_namespace, NamespaceType},
};

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

/// `IndexAllocator` is a helper struct that allocates unique indices for a given type.
/// It keeps tracked of the indices that were freed, and reuses them when possible.
/// You can provide a buffer of indices using the second generic `N` to avoid re-using indices
/// too fast.
/// TODO: Consider just having a simple IndexAllocator that keeps track of last index and increases
/// by 1 as it'd be hard to exhaust u64. (which is usually T)
#[derive(Debug)]
pub struct IndexAllocator<T, const N: usize>
where
    T: Num,
{
    index: T,
    vacant_indices: VecDeque<T>,
}

impl<T, const N: usize> IndexAllocator<T, N>
where
    T: Num + CheckedAdd + Clone,
{
    /// Returns the next available index, returns None if not available (reached max)
    pub fn next_index(&mut self) -> Option<T> {
        if self.vacant_indices.len() > N
            && let Some(i) = self.vacant_indices.pop_front()
        {
            return Some(i);
        }
        match self.index.checked_add(&T::one()) {
            Some(new_index) => {
                let res = self.index.clone();
                self.index = new_index;
                Some(res)
            }
            None => None,
        }
    }

    pub fn free_index(&mut self, index: T) {
        self.vacant_indices.push_back(index)
    }
}

impl<T, const N: usize> Default for IndexAllocator<T, N>
where
    T: Num,
{
    fn default() -> Self {
        Self {
            index: T::zero(),
            vacant_indices: Default::default(),
        }
    }
}

/// Helper that creates a new [`tokio::runtime::Runtime`], and immediately blocks on it.
///
/// Used to start new tasks that would be too heavy for just [`tokio::task::spawn()`] in the
/// caller's runtime.
///
/// These tasks will execute `on_start_fn` to change namespace (see [`enter_namespace`] for more
/// details).
#[tracing::instrument(level = "trace", skip_all)]
pub(crate) fn run_thread<F, StartFn>(
    future: F,
    thread_name: String,
    on_start_fn: StartFn,
) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
    StartFn: Fn() + Send + Sync + 'static,
{
    std::thread::spawn(move || {
        on_start_fn();

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .thread_name(thread_name)
            .on_thread_start(on_start_fn)
            .build()
            .unwrap();
        rt.block_on(future)
    })
}

/// Calls [`run_thread`] with `on_start_fn` always being [`enter_namespace`].
#[tracing::instrument(level = "trace", skip_all)]
pub(crate) fn run_thread_in_namespace<F>(
    future: F,
    thread_name: String,
    pid: Option<u64>,
    namespace: &str,
) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    let namespace = namespace.to_string();

    run_thread(future, thread_name, move || {
        enter_namespace(pid, &namespace).expect("Failed setting namespace!")
    })
}

/// Used to enter a different (so far only used for "net") namespace for a task.
///
/// Many of the agent's TCP/UDP connections require that they're made from the `pid`'s namespace to
/// work.
#[tracing::instrument(level = "trace")]
pub(crate) fn enter_namespace(pid: Option<u64>, namespace: &str) -> Result<(), AgentError> {
    if let Some(pid) = pid {
        Ok(set_namespace(pid, NamespaceType::Net).inspect_err(|fail| {
            error!("Failed setting pid {pid:#?} namespace {namespace:#?} with {fail:#?}")
        })?)
    } else {
        Ok(())
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
mod indexallocator_tests {
    use super::IndexAllocator;

    #[test]
    fn sanity() {
        let mut index_allocator: IndexAllocator<u32, 0> = Default::default();
        let index = index_allocator.next_index().unwrap();
        assert_eq!(0, index);
        let index = index_allocator.next_index().unwrap();
        assert_eq!(1, index);
        index_allocator.free_index(0);
        let index = index_allocator.next_index().unwrap();
        assert_eq!(0, index);
    }

    #[test]
    fn sanity_buffer() {
        let mut index_allocator: IndexAllocator<u32, 2> = Default::default();
        let index = index_allocator.next_index().unwrap();
        assert_eq!(0, index);
        let index = index_allocator.next_index().unwrap();
        assert_eq!(1, index);
        index_allocator.free_index(0);
        let index = index_allocator.next_index().unwrap();
        assert_eq!(2, index);
        let index = index_allocator.next_index().unwrap();
        assert_eq!(3, index);
        index_allocator.free_index(1);
        index_allocator.free_index(2);
        let index = index_allocator.next_index().unwrap();
        assert_eq!(0, index);
    }

    #[test]
    fn check_max() {
        let mut index_allocator: IndexAllocator<u8, 0> = Default::default();
        for _ in 0..=u8::MAX - 1 {
            index_allocator.next_index().unwrap();
        }
        assert!(index_allocator.next_index().is_none());
    }
}
