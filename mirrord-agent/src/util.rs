use std::{
    borrow::Borrow,
    clone::Clone,
    collections::{HashMap, HashSet},
    hash::{Hash, Hasher},
    sync::{Arc, Mutex},
};

use num_traits::{zero, CheckedAdd, Num};

/// Struct that helps you manage topic -> subscribers
/// When a topip has no subscribers, it is removed.
#[derive(Debug)]
pub struct Subscriptions<T, C> {
    _inner: HashMap<T, HashSet<C>>,
}

pub type ClientID = u32;
pub type ReusableClientID = ReusableIndex<u32>;

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
    vacant_indices: Arc<Mutex<Vec<T>>>,
}

impl<T> IndexAllocator<T>
where
    T: Num + CheckedAdd + Clone + Copy,
{
    pub fn new() -> IndexAllocator<T> {
        IndexAllocator {
            index: zero(),
            vacant_indices: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Returns the next available index, returns None if not available (reached max)
    pub fn next_index(&mut self) -> Option<ReusableIndex<T>> {
        if let Some(i) = self.vacant_indices.lock().unwrap().pop() {
            return Some(ReusableIndex::new(i, self.vacant_indices.clone()));
        }
        match self.index.checked_add(&T::one()) {
            Some(new_index) => {
                let res = self.index;
                self.index = new_index;
                Some(ReusableIndex::new(res, self.vacant_indices.clone()))
            }
            None => None,
        }
    }

    pub fn free_index(&mut self, index: T) {
        self.vacant_indices.lock().unwrap().push(index)
    }
}

impl Default for IndexAllocator<usize> {
    fn default() -> Self {
        IndexAllocator::new()
    }
}

#[derive(Debug, Clone)]
pub struct ReusableIndex<T>
where
    T: Num + Copy + Clone + CheckedAdd,
{
    inner: T,
    vacant_indices: Arc<Mutex<Vec<T>>>,
}

impl<T> ReusableIndex<T>
where
    T: Num + Copy + Clone + CheckedAdd,
{
    fn new(value: T, vacant_indices: Arc<Mutex<Vec<T>>>) -> ReusableIndex<T> {
        ReusableIndex {
            inner: value,
            vacant_indices,
        }
    }
}

impl<T> std::ops::Deref for ReusableIndex<T>
where
    T: Num + Copy + Clone + CheckedAdd,
{
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T> Drop for ReusableIndex<T>
where
    T: Num + Copy + Clone + CheckedAdd,
{
    fn drop(&mut self) {
        self.vacant_indices.lock().unwrap().push(self.inner)
    }
}

impl<T: ?Sized + Hash> Hash for ReusableIndex<T>
where
    T: Num + Copy + Clone + CheckedAdd,
{
    fn hash<H: Hasher>(&self, state: &mut H) {
        (**self).hash(state)
    }
}

impl<T> PartialEq for ReusableIndex<T>
where
    T: Num + Copy + Clone + CheckedAdd,
{
    fn eq(&self, other: &Self) -> bool {
        self.inner == other.inner
    }
}
impl<T> Eq for ReusableIndex<T> where T: Num + Copy + Clone + CheckedAdd {}

impl<T> Borrow<T> for ReusableIndex<T>
where
    T: Num + Copy + Clone + CheckedAdd,
{
    fn borrow(&self) -> &T {
        &self.inner
    }
}

// struct ReusableIndex<T> where T: Num + Copy + Clone + CheckedAdd {
//     inner: T,
//     allocator: Mutex<IndexAllocator<T>>
// }

// impl<T> std::ops::Deref for ReusableIndex<T> where T: Num + Copy + Clone + CheckedAdd {
//     type Target = T;

//     fn deref(&self) -> &Self::Target {
//         &self.inner
//     }
// }

// impl<T> Drop for ReusableIndex<T> where T: Num + Copy + Clone + CheckedAdd {
//     fn drop(&mut self) {
//         self.allocator.get_mut().unwrap().free_index(self.inner)
//     }
// }

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
        assert_eq!(0, *index);
        let index = index_allocator.next_index().unwrap();
        assert_eq!(0, *index);
        let index2 = index_allocator.next_index().unwrap();
        assert_eq!(1, *index2);
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
