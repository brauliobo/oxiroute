//! Basic credentials that already passed a password check. A client sends the same `Authorization` header on every
//! request; the hash (bcrypt or APR1, thousands of rounds by design) is paid once, not once per request.
//!
//! Only a success is remembered, so a wrong password costs the full hash every time, and the entry is the keyed digest
//! of the exact `user:password` bytes: it answers for a client that already knows the password and for nobody else.
//! The table belongs to one loaded password file, so a reload (a new generation) starts empty.

use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

use openssl::{rand::rand_bytes, sha::sha256};

/// Remembered credentials; the table is emptied when it is full.
const CAPACITY: usize = 1024;

pub(super) struct VerifiedCredentials {
    key: [u8; 32],
    users: RwLock<HashMap<[u8; 32], Arc<str>>>,
}

impl VerifiedCredentials {
    pub(super) fn new() -> Self {
        let mut key = [0; 32];
        rand_bytes(&mut key).expect("the system random generator is available");
        Self {
            key,
            users: RwLock::new(HashMap::new()),
        }
    }

    fn digest(&self, credentials: &[u8]) -> [u8; 32] {
        sha256(&[&self.key[..], credentials].concat())
    }

    pub(super) fn recall(&self, credentials: &[u8]) -> Option<Arc<str>> {
        let users = self
            .users
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        users.get(&self.digest(credentials)).cloned()
    }

    pub(super) fn remember(&self, credentials: &[u8], user: &Arc<str>) {
        let mut users = self
            .users
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if users.len() >= CAPACITY {
            users.clear();
        }
        users.insert(self.digest(credentials), Arc::clone(user));
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.users
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_remembered_credential_is_recalled_by_its_exact_bytes() {
        let verified = VerifiedCredentials::new();
        let user: Arc<str> = "alpha".into();
        verified.remember(b"alpha:pass", &user);
        assert_eq!(verified.recall(b"alpha:pass").as_deref(), Some("alpha"));
        assert_eq!(verified.recall(b"alpha:pas"), None);
        assert_eq!(verified.recall(b"alpha:pass "), None);
        assert_eq!(verified.recall(b"Alpha:pass"), None);
    }

    #[test]
    fn two_tables_do_not_recall_each_other() {
        let (first, second) = (VerifiedCredentials::new(), VerifiedCredentials::new());
        first.remember(b"alpha:pass", &"alpha".into());
        assert_eq!(second.recall(b"alpha:pass"), None);
    }

    #[test]
    fn a_full_table_starts_over_instead_of_growing() {
        let verified = VerifiedCredentials::new();
        let user: Arc<str> = "alpha".into();
        for index in 0..CAPACITY {
            verified.remember(format!("alpha:{index}").as_bytes(), &user);
        }
        assert_eq!(verified.len(), CAPACITY);
        verified.remember(b"alpha:next", &user);
        assert_eq!(verified.len(), 1);
        assert_eq!(verified.recall(b"alpha:next").as_deref(), Some("alpha"));
    }
}
