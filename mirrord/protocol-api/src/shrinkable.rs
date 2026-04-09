use std::{
    collections::{HashMap, HashSet, VecDeque},
    hash::{BuildHasher, Hash},
};

/// Util trait allowing for shrinking collections in a smart way.
pub trait Shrinkable {
    /// Shrinks this collection by half if at least 3/4 of its capacity is unused,
    /// and capacity is greater than 4.
    fn smart_shrink(&mut self) {
        if self.len() <= self.capacity() / 4 && self.capacity() > 4 {
            self.shrink_to(self.capacity() / 2);
        }
    }

    fn len(&self) -> usize;

    fn is_empty(&self) -> bool;

    fn capacity(&self) -> usize;

    fn shrink_to(&mut self, capacity: usize);
}

impl<T> Shrinkable for Vec<T> {
    fn len(&self) -> usize {
        self.len()
    }

    fn is_empty(&self) -> bool {
        self.is_empty()
    }

    fn capacity(&self) -> usize {
        self.capacity()
    }

    fn shrink_to(&mut self, capacity: usize) {
        self.shrink_to(capacity);
    }
}

impl<T> Shrinkable for VecDeque<T> {
    fn len(&self) -> usize {
        self.len()
    }

    fn is_empty(&self) -> bool {
        self.is_empty()
    }

    fn capacity(&self) -> usize {
        self.capacity()
    }

    fn shrink_to(&mut self, capacity: usize) {
        self.shrink_to(capacity);
    }
}

impl<K: Hash + Eq, V, S: BuildHasher> Shrinkable for HashMap<K, V, S> {
    fn len(&self) -> usize {
        self.len()
    }

    fn is_empty(&self) -> bool {
        self.is_empty()
    }

    fn capacity(&self) -> usize {
        self.capacity()
    }

    fn shrink_to(&mut self, capacity: usize) {
        self.shrink_to(capacity);
    }
}

impl<K: Hash + Eq, S: BuildHasher> Shrinkable for HashSet<K, S> {
    fn len(&self) -> usize {
        self.len()
    }

    fn is_empty(&self) -> bool {
        self.is_empty()
    }

    fn capacity(&self) -> usize {
        self.capacity()
    }

    fn shrink_to(&mut self, capacity: usize) {
        self.shrink_to(capacity);
    }
}
