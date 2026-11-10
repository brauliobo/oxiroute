//! Resumption state with the profile's session timeout: rustls bounds neither its in-memory cache nor its tickets by age.

use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, PoisonError},
    time::{SystemTime, UNIX_EPOCH},
};

use rustls::server::{ProducesTickets, StoresServerSessions};

const STAMP_BYTES: usize = 8;

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

fn stamped(plain: &[u8]) -> Vec<u8> {
    let mut stamped = Vec::with_capacity(STAMP_BYTES + plain.len());
    stamped.extend_from_slice(&now().to_be_bytes());
    stamped.extend_from_slice(plain);
    stamped
}

/// The plaintext of `stamped` while it is younger than `timeout` seconds.
fn fresh(stamped: &[u8], timeout: u32) -> Option<&[u8]> {
    let (created, plain) = stamped.split_at_checked(STAMP_BYTES)?;
    let age = now().saturating_sub(u64::from_be_bytes(created.try_into().ok()?));
    (age <= u64::from(timeout)).then_some(plain)
}

/// Stateless tickets that stop resuming after the session timeout. The ticket keys rotate on rustls' schedule; the
/// stamp inside the encrypted ticket is what limits a ticket's age.
#[derive(Debug)]
pub(super) struct TimedTicketer {
    keys: Arc<dyn ProducesTickets>,
    timeout: u32,
}

impl TimedTicketer {
    pub(super) fn new(keys: Arc<dyn ProducesTickets>, timeout: u32) -> Self {
        Self { keys, timeout }
    }
}

impl ProducesTickets for TimedTicketer {
    fn enabled(&self) -> bool {
        self.keys.enabled()
    }

    fn lifetime(&self) -> u32 {
        self.timeout.min(self.keys.lifetime())
    }

    fn encrypt(&self, plain: &[u8]) -> Option<Vec<u8>> {
        self.keys.encrypt(&stamped(plain))
    }

    fn decrypt(&self, cipher: &[u8]) -> Option<Vec<u8>> {
        let stamped = self.keys.decrypt(cipher)?;
        fresh(&stamped, self.timeout).map(<[u8]>::to_vec)
    }
}

/// The profile's named session cache: at most `capacity` sessions, the oldest leaving first, none older than the
/// timeout. It grows with use, so a large configured size costs memory only once the sessions exist.
#[derive(Debug)]
pub(super) struct SessionCache {
    entries: Mutex<Entries>,
    capacity: usize,
    timeout: u32,
}

#[derive(Debug, Default)]
struct Entries {
    sessions: HashMap<Vec<u8>, Vec<u8>>,
    oldest: VecDeque<Vec<u8>>,
}

impl SessionCache {
    pub(super) fn new(capacity: usize, timeout: u32) -> Self {
        Self {
            entries: Mutex::default(),
            capacity,
            timeout,
        }
    }

    fn entries(&self) -> std::sync::MutexGuard<'_, Entries> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl StoresServerSessions for SessionCache {
    fn put(&self, key: Vec<u8>, value: Vec<u8>) -> bool {
        let mut entries = self.entries();
        if entries.sessions.insert(key.clone(), stamped(&value)).is_none() {
            entries.oldest.push_back(key);
        }
        while entries.sessions.len() > self.capacity {
            let Some(oldest) = entries.oldest.pop_front() else {
                break;
            };
            entries.sessions.remove(&oldest);
        }
        true
    }

    fn get(&self, key: &[u8]) -> Option<Vec<u8>> {
        let entries = self.entries();
        fresh(entries.sessions.get(key)?, self.timeout).map(<[u8]>::to_vec)
    }

    fn take(&self, key: &[u8]) -> Option<Vec<u8>> {
        let mut entries = self.entries();
        let stamped = entries.sessions.remove(key)?;
        fresh(&stamped, self.timeout).map(<[u8]>::to_vec)
    }

    fn can_cache(&self) -> bool {
        self.capacity > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct Plain;

    impl ProducesTickets for Plain {
        fn enabled(&self) -> bool {
            true
        }

        fn lifetime(&self) -> u32 {
            43_200
        }

        fn encrypt(&self, plain: &[u8]) -> Option<Vec<u8>> {
            Some(plain.to_vec())
        }

        fn decrypt(&self, cipher: &[u8]) -> Option<Vec<u8>> {
            Some(cipher.to_vec())
        }
    }

    #[test]
    fn a_ticket_resumes_within_the_timeout_and_advertises_it() {
        let tickets = TimedTicketer::new(Arc::new(Plain), 300);

        assert_eq!(tickets.lifetime(), 300);
        let ticket = tickets.encrypt(b"session").unwrap();
        assert_eq!(tickets.decrypt(&ticket).unwrap(), b"session");
    }

    #[test]
    fn an_older_ticket_is_refused() {
        let tickets = TimedTicketer::new(Arc::new(Plain), 300);
        let mut old = (now() - 301).to_be_bytes().to_vec();
        old.extend_from_slice(b"session");

        assert_eq!(tickets.decrypt(&old), None);
        assert_eq!(tickets.decrypt(b"short"), None);
    }

    #[test]
    fn the_cache_keeps_the_newest_sessions_within_its_capacity() {
        let cache = SessionCache::new(2, 300);

        for key in [b"a", b"b", b"c"] {
            assert!(cache.put(key.to_vec(), key.to_vec()));
        }

        assert_eq!(cache.get(b"a"), None);
        assert_eq!(cache.get(b"b").unwrap(), b"b");
        assert_eq!(cache.take(b"c").unwrap(), b"c");
        assert_eq!(cache.take(b"c"), None);
    }

    #[test]
    fn the_cache_refuses_a_session_past_the_timeout() {
        let cache = SessionCache::new(2, 0);
        let mut expired = (now() - 5).to_be_bytes().to_vec();
        expired.extend_from_slice(b"session");
        cache.entries().sessions.insert(b"key".to_vec(), expired);

        assert_eq!(cache.get(b"key"), None);
        assert_eq!(cache.take(b"key"), None);
    }
}
