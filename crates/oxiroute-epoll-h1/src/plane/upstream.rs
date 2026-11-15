use std::{
    collections::HashMap,
    hash::{BuildHasherDefault, Hasher},
};

use super::{clock::Moment, link::Link, service::Service, slab::Token};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Phase {
    Connecting,
    Busy,
    Idle,
}

pub(crate) struct Upstream<S: Service> {
    pub(crate) link: Link,
    pub(crate) key: u64,
    pub(crate) phase: Phase,
    pub(crate) client: Option<Token>,
    pub(crate) deadline: Option<Moment>,
    pub(crate) _lease: S::Lease,
    pub(crate) idle: Option<std::time::Duration>,
}

#[derive(Default)]
pub(crate) struct KeyHasher(u64);

impl Hasher for KeyHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 = self.0.rotate_left(8) ^ u64::from(*byte);
        }
    }

    fn write_u64(&mut self, value: u64) {
        self.0 = value;
    }
}

pub(crate) struct IdlePool {
    by_key: HashMap<u64, Vec<Token>, BuildHasherDefault<KeyHasher>>,
    total: usize,
    max_total: usize,
    max_per_key: usize,
}

impl IdlePool {
    pub(crate) fn new(max_total: usize, max_per_key: usize) -> Self {
        Self {
            by_key: HashMap::default(),
            total: 0,
            max_total,
            max_per_key,
        }
    }

    pub(crate) fn take(&mut self, key: u64) -> Option<Token> {
        let token = self.by_key.get_mut(&key)?.pop()?;
        self.total -= 1;
        Some(token)
    }

    pub(crate) fn put(&mut self, key: u64, token: Token) -> bool {
        if self.total >= self.max_total {
            return false;
        }
        let capacity = self.max_per_key;
        let idle = self
            .by_key
            .entry(key)
            .or_insert_with(|| Vec::with_capacity(capacity));
        if idle.len() >= capacity {
            return false;
        }
        idle.push(token);
        self.total += 1;
        true
    }

    pub(crate) fn remove(&mut self, key: u64, token: Token) {
        let Some(idle) = self.by_key.get_mut(&key) else {
            return;
        };
        if let Some(at) = idle.iter().position(|idle| *idle == token) {
            idle.swap_remove(at);
            self.total -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(index: u32) -> Token {
        Token {
            index,
            generation: 0,
        }
    }

    #[test]
    fn the_most_recently_returned_connection_is_taken_first() {
        let mut pool = IdlePool::new(8, 8);
        assert!(pool.put(1, token(10)));
        assert!(pool.put(1, token(11)));
        assert!(pool.put(2, token(12)));
        assert_eq!(pool.take(1), Some(token(11)));
        assert_eq!(pool.take(2), Some(token(12)));
        assert_eq!(pool.take(2), None);
        assert_eq!(pool.take(1), Some(token(10)));
    }

    #[test]
    fn the_pool_refuses_connections_over_its_bounds_and_forgets_removed_ones() {
        let mut pool = IdlePool::new(3, 2);
        assert!(pool.put(1, token(1)));
        assert!(pool.put(1, token(2)));
        assert!(!pool.put(1, token(3)));
        assert!(pool.put(2, token(4)));
        assert!(!pool.put(3, token(5)));
        pool.remove(1, token(1));
        assert!(pool.put(3, token(5)));
        assert_eq!(pool.take(1), Some(token(2)));
        assert_eq!(pool.take(1), None);
    }
}
