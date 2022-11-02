use std::{
    clone::Clone,
    collections::{HashMap, HashSet},
    future::Future,
    hash::Hash,
    thread::JoinHandle,
};

use num_traits::{zero, CheckedAdd, Num, NumCast};

/// Struct that helps you manage topic -> subscribers
/// When a topip has no subscribers, it is removed.
#[derive(Debug)]
pub struct Subscriptions<T, C> {
    _inner: HashMap<T, HashSet<C>>,
}

pub type ClientID = u32;

impl<T, C> Subscriptions<T, C>
where
    T: Eq + Hash + Clone + Copy,
    C: Eq + Hash + Clone + Copy,
{
    pub fn new() -> Subscriptions<T, C> {
        Subscriptions {
            _inner: HashMap::new(),
        }
    }

    /// Add a new subscription to a topic for a given client.
    pub fn subscribe(&mut self, client: C, topic: T) {
        self._inner
            .entry(topic)
            .or_insert_with(HashSet::new)
            .insert(client);
    }

    /// Remove a subscription of given client from the topic.
    /// topic is removed if no subscribers left.
    pub fn unsubscribe(&mut self, client: C, topic: T) {
        if let Some(set) = self._inner.get_mut(&topic) {
            set.remove(&client);
            if set.is_empty() {
                self._inner.remove(&topic);
            }
        }
    }

    /// Get a vector of clients subscribed to a specific topic
    pub fn get_topic_subscribers(&self, topic: T) -> Vec<C> {
        match self._inner.get(&topic) {
            Some(clients_set) => clients_set.iter().cloned().collect(),
            None => Vec::new(),
        }
    }

    /// Get subscribed topics
    pub fn get_subscribed_topics(&self) -> Vec<T> {
        self._inner.keys().cloned().collect()
    }

    /// Get topics subscribed by a client
    pub fn get_client_topics(&self, client: C) -> Vec<T> {
        let mut result = Vec::new();
        for (topic, client_set) in self._inner.iter() {
            if client_set.contains(&client) {
                result.push(*topic)
            }
        }
        result
    }

    /// Remove all subscriptions of a client
    pub fn remove_client(&mut self, client: C) {
        let topics = self.get_client_topics(client);
        for topic in topics {
            self.unsubscribe(client, topic)
        }
    }

    /// Removes a topic and all of it's clients
    #[allow(dead_code)] // we might want it later on
    pub fn remove_topic(&mut self, topic: T) {
        self._inner.remove(&topic);
    }
}

#[derive(Debug)]
pub struct IndexAllocator<T>
where
    T: Num,
{
    index: T,
    vacant_indices: Vec<T>,
}

impl<T> IndexAllocator<T>
where
    T: Num + CheckedAdd + NumCast + Clone,
{
    pub fn new() -> IndexAllocator<T> {
        IndexAllocator {
            index: zero(),
            vacant_indices: Vec::new(),
        }
    }

    /// Returns the next available index, returns None if not available (reached max)
    pub fn next_index(&mut self) -> Option<T> {
        if let Some(i) = self.vacant_indices.pop() {
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
        self.vacant_indices.push(index)
    }
}

impl Default for IndexAllocator<usize> {
    fn default() -> Self {
        IndexAllocator::new()
    }
}

pub fn run_thread<T>(future: T) -> JoinHandle<T::Output>
where
    T: Future + Send + 'static,
    T::Output: Send + 'static,
{
    std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(future)
    })
}

#[cfg(test)]
mod subscription_tests {
    use mirrord_protocol::Port;

    use super::Subscriptions;

    #[test]
    fn sanity() {
        let mut subscriptions = Subscriptions::<Port, _>::new();
        subscriptions.subscribe(3, 2);
        subscriptions.subscribe(3, 3);
        subscriptions.subscribe(3, 1);
        subscriptions.subscribe(1, 4);
        subscriptions.subscribe(2, 1);
        subscriptions.subscribe(2, 1);
        subscriptions.subscribe(3, 1);
        let mut subscribers = subscriptions.get_topic_subscribers(1);
        subscribers.sort();
        assert_eq!(subscribers, vec![2, 3]);

        assert_eq!(subscriptions.get_topic_subscribers(4), vec![1]);
        assert_eq!(subscriptions.get_topic_subscribers(10), Vec::<u32>::new());
        let mut topics = subscriptions.get_subscribed_topics();
        topics.sort();
        assert_eq!(topics, vec![1, 2, 3, 4]);
        topics = subscriptions.get_client_topics(3);
        topics.sort();
        assert_eq!(topics, vec![1, 2, 3]);
        subscriptions.unsubscribe(3, 1);
        assert_eq!(subscriptions.get_topic_subscribers(1), vec![2]);
        subscriptions.unsubscribe(1, 4);
        assert_eq!(subscriptions.get_topic_subscribers(4), Vec::<u32>::new());
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
        let mut index_allocator = IndexAllocator::<u32>::new();
        let index = index_allocator.next_index().unwrap();
        assert_eq!(0, index);
        let index = index_allocator.next_index().unwrap();
        assert_eq!(1, index);
        index_allocator.free_index(0);
        let index = index_allocator.next_index().unwrap();
        assert_eq!(0, index);
    }

    #[test]
    fn check_max() {
        let mut index_allocator = IndexAllocator::<u8>::new();
        for _ in 0..=u8::MAX - 1 {
            index_allocator.next_index().unwrap();
        }
        assert!(index_allocator.next_index().is_none());
    }
}
