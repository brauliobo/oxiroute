use std::{
    collections::BTreeMap,
    fmt,
    sync::{
        Arc, RwLock,
        atomic::{AtomicU64, Ordering},
    },
};

pub const MAX_CHALLENGES: usize = 1_024;
pub const MAX_CHALLENGE_TTL_SECONDS: u64 = 3_600;
const MAX_TOKEN_BYTES: usize = 256;
const MAX_KEY_AUTHORIZATION_BYTES: usize = 512;
const CHALLENGE_PATH_PREFIX: &str = "/.well-known/acme-challenge/";
static NEXT_LEASE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Eq, PartialEq)]
pub struct ChallengeRecord {
    pub token: String,
    pub key_authorization: String,
    pub account_id: String,
    pub order_id: String,
    pub authorization_id: String,
    pub challenge_id: String,
    pub created_at_unix_seconds: u64,
    pub expires_at_unix_seconds: u64,
}

impl fmt::Debug for ChallengeRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChallengeRecord")
            .field("token", &"REDACTED")
            .field("key_authorization", &"REDACTED")
            .field("account_id", &self.account_id)
            .field("order_id", &self.order_id)
            .field("authorization_id", &self.authorization_id)
            .field("challenge_id", &self.challenge_id)
            .field("created_at_unix_seconds", &self.created_at_unix_seconds)
            .field("expires_at_unix_seconds", &self.expires_at_unix_seconds)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChallengeHttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
    pub content_type: &'static str,
    pub cache_control: &'static str,
}

impl ChallengeHttpResponse {
    fn not_found() -> Self {
        Self {
            status: 404,
            body: Vec::new(),
            content_type: "text/plain; charset=utf-8",
            cache_control: "no-store",
        }
    }

    fn found(body: &str, head: bool) -> Self {
        Self {
            status: 200,
            body: if head {
                Vec::new()
            } else {
                body.as_bytes().to_vec()
            },
            content_type: "text/plain; charset=utf-8",
            cache_control: "no-store",
        }
    }
}

#[derive(Clone)]
pub struct ChallengeStore {
    inner: Arc<RwLock<BTreeMap<String, StoredChallenge>>>,
    capacity: usize,
}

struct StoredChallenge {
    record: ChallengeRecord,
    owner_id: u64,
}

impl fmt::Debug for ChallengeStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len();
        formatter
            .debug_struct("ChallengeStore")
            .field("count", &count)
            .field("capacity", &self.capacity)
            .finish()
    }
}

impl Default for ChallengeStore {
    fn default() -> Self {
        Self::new(MAX_CHALLENGES)
    }
}

impl ChallengeStore {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(BTreeMap::new())),
            capacity: capacity.min(MAX_CHALLENGES),
        }
    }

    /// Provisions one exact-token challenge and returns a cleanup lease.
    ///
    /// # Errors
    ///
    /// Returns an error when the record is malformed, expired, duplicated, or the bounded store is
    /// full.
    pub fn provision(
        &self,
        record: ChallengeRecord,
    ) -> Result<ChallengeLease, ChallengeStoreError> {
        validate_record(&record)?;
        let mut entries = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.retain(|_, stored| {
            stored.record.expires_at_unix_seconds > record.created_at_unix_seconds
        });
        if entries.contains_key(&record.token) {
            return Err(ChallengeStoreError::DuplicateToken);
        }
        if entries.len() >= self.capacity {
            return Err(ChallengeStoreError::CapacityExceeded);
        }
        let token = record.token.clone();
        let owner_id = NEXT_LEASE_ID.fetch_add(1, Ordering::Relaxed);
        entries.insert(token.clone(), StoredChallenge { record, owner_id });
        drop(entries);
        Ok(ChallengeLease {
            store: self.clone(),
            token: Some(token),
            owner_id,
        })
    }

    /// Returns key authorization for an exact token while retaining no order details.
    #[must_use]
    pub fn lookup(&self, token: &str, now_unix_seconds: u64) -> Option<String> {
        if !valid_token(token) {
            return None;
        }
        let mut entries = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let expired = entries
            .get(token)
            .is_some_and(|stored| now_unix_seconds >= stored.record.expires_at_unix_seconds);
        if expired {
            entries.remove(token);
            return None;
        }
        entries
            .get(token)
            .map(|stored| stored.record.key_authorization.clone())
    }

    /// Handles only the exact ACME challenge path before normal routing.
    ///
    /// A path under the challenge prefix is always handled, including malformed or unknown tokens;
    /// those cases return an empty 404 without revealing challenge state.
    #[must_use]
    pub fn route(
        &self,
        method: &str,
        path: &str,
        now_unix_seconds: u64,
    ) -> Option<ChallengeHttpResponse> {
        let token = path.strip_prefix(CHALLENGE_PATH_PREFIX)?;
        if token.is_empty() || token.contains('/') || token.contains('?') || !valid_token(token) {
            return Some(ChallengeHttpResponse::not_found());
        }
        let head = method == "HEAD";
        if method != "GET" && !head {
            return Some(ChallengeHttpResponse::not_found());
        }
        Some(
            self.lookup(token, now_unix_seconds)
                .map_or_else(ChallengeHttpResponse::not_found, |key_authorization| {
                    ChallengeHttpResponse::found(&key_authorization, head)
                }),
        )
    }

    /// Removes one token without exposing whether its associated order existed.
    pub fn cancel(&self, token: &str) -> bool {
        self.inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(token)
            .is_some()
    }

    /// Removes all records for one opaque authorization identifier.
    #[must_use]
    pub fn cancel_authorization(&self, authorization_id: &str) -> usize {
        self.cancel_matching(|record| record.authorization_id == authorization_id)
    }

    /// Removes all records for one opaque order identifier.
    #[must_use]
    pub fn cancel_order(&self, order_id: &str) -> usize {
        self.cancel_matching(|record| record.order_id == order_id)
    }

    /// Removes expired records and returns the number of records cleaned.
    #[must_use]
    pub fn reap_expired(&self, now_unix_seconds: u64) -> usize {
        self.cancel_matching(|record| record.expires_at_unix_seconds <= now_unix_seconds)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn cancel_matching(&self, predicate: impl Fn(&ChallengeRecord) -> bool) -> usize {
        let mut entries = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let before = entries.len();
        entries.retain(|_, stored| !predicate(&stored.record));
        before - entries.len()
    }

    fn cancel_owned(&self, token: &str, owner_id: u64) -> bool {
        let mut entries = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if entries
            .get(token)
            .is_some_and(|stored| stored.owner_id == owner_id)
        {
            entries.remove(token);
            true
        } else {
            false
        }
    }
}

pub struct ChallengeLease {
    store: ChallengeStore,
    token: Option<String>,
    owner_id: u64,
}

impl fmt::Debug for ChallengeLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ChallengeLease")
            .field("active", &self.token.is_some())
            .finish_non_exhaustive()
    }
}

impl ChallengeLease {
    #[must_use]
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// Completes a challenge and removes its material immediately.
    pub fn complete(mut self) {
        self.cancel_inner();
    }

    /// Cancels a challenge and removes its material immediately.
    pub fn cancel(mut self) {
        self.cancel_inner();
    }

    fn cancel_inner(&mut self) {
        if let Some(token) = self.token.take() {
            self.store.cancel_owned(&token, self.owner_id);
        }
    }
}

impl Drop for ChallengeLease {
    fn drop(&mut self) {
        self.cancel_inner();
    }
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum ChallengeStoreError {
    #[error("HTTP-01 challenge record is invalid")]
    InvalidRecord,
    #[error("HTTP-01 challenge token is already provisioned")]
    DuplicateToken,
    #[error("HTTP-01 challenge store capacity is exhausted")]
    CapacityExceeded,
}

fn validate_record(record: &ChallengeRecord) -> Result<(), ChallengeStoreError> {
    if !valid_token(&record.token)
        || record.key_authorization.is_empty()
        || record.key_authorization.len() > MAX_KEY_AUTHORIZATION_BYTES
        || !record.key_authorization.is_ascii()
        || record.key_authorization.bytes().any(|byte| byte <= b' ')
        || record.account_id.is_empty()
        || record.order_id.is_empty()
        || record.authorization_id.is_empty()
        || record.challenge_id.is_empty()
        || !valid_opaque_id(&record.account_id)
        || !valid_opaque_id(&record.order_id)
        || !valid_opaque_id(&record.authorization_id)
        || !valid_opaque_id(&record.challenge_id)
        || record.expires_at_unix_seconds <= record.created_at_unix_seconds
        || record
            .expires_at_unix_seconds
            .saturating_sub(record.created_at_unix_seconds)
            > MAX_CHALLENGE_TTL_SECONDS
    {
        return Err(ChallengeStoreError::InvalidRecord);
    }
    Ok(())
}

fn valid_token(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= MAX_TOKEN_BYTES
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn valid_opaque_id(value: &str) -> bool {
    value.len() <= 256
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(token: &str, expires_at_unix_seconds: u64) -> ChallengeRecord {
        ChallengeRecord {
            token: token.into(),
            key_authorization: format!("{token}.thumbprint"),
            account_id: "acct-1".into(),
            order_id: "order-1".into(),
            authorization_id: "authz-1".into(),
            challenge_id: "challenge-1".into(),
            created_at_unix_seconds: 1,
            expires_at_unix_seconds,
        }
    }

    #[test]
    fn exact_token_route_wins_and_unknown_values_are_empty_not_found() {
        let store = ChallengeStore::default();
        let lease = store
            .provision(record("abc_DEF-1", 100))
            .expect("provision");
        assert_eq!(
            store
                .route("GET", "/.well-known/acme-challenge/abc_DEF-1", 50)
                .expect("challenge route")
                .status,
            200
        );
        assert_eq!(
            store
                .route("GET", "/.well-known/acme-challenge/unknown", 50)
                .expect("unknown challenge route"),
            ChallengeHttpResponse::not_found()
        );
        assert_eq!(
            store
                .route("GET", "/.well-known/acme-challenge/abc_DEF-1/extra", 50)
                .expect("malformed challenge route"),
            ChallengeHttpResponse::not_found()
        );
        assert!(store.route("GET", "/normal", 50).is_none());
        lease.complete();
        assert!(store.is_empty());
    }

    #[test]
    fn head_returns_no_secret_body_but_get_returns_only_key_authorization() {
        let store = ChallengeStore::default();
        let _lease = store.provision(record("token", 100)).expect("provision");
        let head = store
            .route("HEAD", "/.well-known/acme-challenge/token", 50)
            .expect("HEAD");
        assert_eq!(head.status, 200);
        assert!(head.body.is_empty());
        let get = store
            .route("GET", "/.well-known/acme-challenge/token", 50)
            .expect("GET");
        assert_eq!(get.body, b"token.thumbprint");
        assert!(!format!("{store:?}").contains("thumbprint"));
    }

    #[test]
    fn expiry_and_cancellation_are_idempotent_and_bounded() {
        let store = ChallengeStore::new(1);
        let lease = store.provision(record("one", 10)).expect("provision");
        assert!(matches!(
            store.provision(record("two", 20)),
            Err(ChallengeStoreError::CapacityExceeded)
        ));
        assert_eq!(store.reap_expired(10), 1);
        assert_eq!(store.reap_expired(11), 0);
        drop(lease);
        assert!(store.is_empty());
    }

    #[test]
    fn expired_capacity_is_reclaimed_and_old_leases_cannot_cancel_replacements() {
        let store = ChallengeStore::new(1);
        let old = store.provision(record("token", 10)).expect("old challenge");
        assert!(store.provision(record("replacement", 20)).is_err());
        let replacement = store
            .provision(record("replacement", 20))
            .expect_err("capacity remains occupied before expiry");
        assert_eq!(replacement, ChallengeStoreError::CapacityExceeded);
        assert_eq!(store.reap_expired(10), 1);
        let replacement = store
            .provision(record("token", 30))
            .expect("replacement challenge");
        old.cancel();
        assert_eq!(
            store.lookup("token", 11).as_deref(),
            Some("token.thumbprint")
        );
        replacement.cancel();
        assert!(store.is_empty());
    }

    #[test]
    fn challenge_debug_redacts_token_and_key_authorization() {
        let record = record("secret-token", 100);
        let debug = format!("{record:?}");
        assert!(!debug.contains("secret-token"));
        assert!(!debug.contains("secret-token.thumbprint"));
        assert!(debug.contains("REDACTED"));
    }

    #[test]
    fn cleanup_can_target_authorization_without_revealing_other_records() {
        let store = ChallengeStore::default();
        let first = store.provision(record("first", 100)).expect("first");
        let mut second_record = record("second", 100);
        second_record.authorization_id = "authz-2".into();
        let second = store.provision(second_record).expect("second");
        assert_eq!(store.cancel_authorization("authz-1"), 1);
        assert!(store.lookup("first", 50).is_none());
        assert_eq!(
            store.lookup("second", 50).as_deref(),
            Some("second.thumbprint")
        );
        first.cancel();
        second.cancel();
    }
}
