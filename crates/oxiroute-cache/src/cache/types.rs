#[allow(clippy::wildcard_imports)]
use super::*;

/// Immutable insertion candidate produced only after policy and bound checks.
#[derive(Clone, Debug)]
pub struct PreparedEntry {
    pub(super) identity: Arc<CacheIdentity>,
    pub(crate) key: CacheKey,
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Bytes,
    pub(crate) tags: Vec<Bytes>,
    pub(crate) policy: ResponsePolicy,
    pub(crate) validators: Validators,
    pub(crate) response_received: MonoTime,
    pub(crate) charge: usize,
}

pub(crate) struct RecoveredEntry {
    pub(crate) key: CacheKey,
    pub(crate) status: StatusCode,
    pub(crate) headers: HeaderMap,
    pub(crate) body: Bytes,
    pub(crate) tags: Vec<Bytes>,
    pub(crate) policy: ResponsePolicy,
    pub(crate) validators: Validators,
    pub(crate) response_received: MonoTime,
    pub(crate) charge: usize,
}

/// Complete bounded response input used for canonical timeline preparation.
pub struct CacheResponse<'a> {
    pub status: StatusCode,
    pub headers: &'a HeaderMap,
    pub body: Bytes,
    pub timing: ResponseTiming,
    pub tags: &'a [&'a [u8]],
}

impl PreparedEntry {
    #[must_use]
    pub const fn key(&self) -> &CacheKey {
        &self.key
    }

    #[must_use]
    pub const fn charge(&self) -> usize {
        self.charge
    }
}

/// A cache response snapshot. HEAD callers use the same object but suppress `body` on the wire.
#[derive(Clone, Debug)]
pub struct CachedResponse {
    pub key: CacheKey,
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Bytes,
    pub age: Duration,
}

/// Cache lookup result and wire-facing cache status.
#[derive(Clone, Debug)]
pub enum Lookup {
    Bypass {
        status: LookupStatus,
    },
    Miss {
        status: LookupStatus,
        only_if_cached: bool,
        base: BaseKey,
    },
    Hit {
        status: LookupStatus,
        response: CachedResponse,
    },
    Revalidate {
        status: LookupStatus,
        response: CachedResponse,
        validators: Validators,
        stale_if_error: bool,
    },
}

/// Stable status vocabulary suitable for `Cache-Status` or access-log mapping.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LookupStatus {
    Bypass,
    Miss,
    Hit,
    StaleWhileRevalidate,
    Stale,
    Revalidate,
    StaleIfError,
}

/// Monotonic counters and current bounded resource use.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    pub bypasses: u64,
    pub revalidations: u64,
    pub stale_while_revalidate: u64,
    pub stale_hits: u64,
    pub stale_if_error: u64,
    pub stores: u64,
    pub rejected: u64,
    pub evictions: u64,
    pub purged: u64,
    pub fill_leaders: u64,
    pub fill_followers: u64,
    pub fill_cancelled: u64,
    pub entries: usize,
    pub bytes_used: usize,
    pub in_flight: usize,
}

/// Result of joining a collapsed-forwarding group.
pub enum FillJoin {
    Leader(FillGuard),
    Follower(FillWaiter),
    AtCapacity,
}

/// Generation-bound leader permit. Dropping it cancels the fill and wakes followers.
pub struct FillGuard {
    pub(super) shared: Weak<Shared>,
    pub(super) identity: Arc<CacheIdentity>,
    pub(super) base: BaseKey,
    pub(super) generation: Arc<FlightGeneration>,
    pub(super) active: bool,
}

pub(crate) struct ClaimedEntry(pub(super) PreparedEntry);

#[derive(Debug, Eq, PartialEq)]
pub(crate) struct InsertionDelta {
    pub(crate) inserted: CacheKey,
    pub(crate) removed: Vec<CacheKey>,
    pub(crate) evicted: usize,
}

pub(crate) enum ClaimedStoreOutcome {
    Stored(InsertionDelta),
    GenerationLost,
}

/// Follower notification handle. It contains no cache lock and is safe to await across I/O.
pub struct FillWaiter {
    pub(super) receiver: watch::Receiver<FillOutcome>,
}

/// Terminal result broadcast to collapsed followers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FillOutcome {
    Filling,
    Stored,
    NotStored,
    Cancelled,
    Purged,
}

/// Insertion result from a generation-valid leader.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoreOutcome {
    Stored { evicted: usize },
    GenerationLost,
}

/// Number of objects removed by a bounded purge operation.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PurgeResult {
    pub entries: usize,
    pub bytes: usize,
    pub fills_cancelled: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("cache configuration limits must be nonzero and internally consistent")]
    InvalidConfig,
    #[error("invalid request cache metadata")]
    InvalidRequest(#[source] ParseError),
    #[error(transparent)]
    InvalidKey(#[from] KeyError),
    #[error("response cannot be stored in the shared cache")]
    ResponseRejected(#[from] ResponseRejection),
    #[error("only GET responses can create cached representations")]
    HeadOrUnsupportedMethod,
    #[error("response headers exceed configured bounds")]
    HeadersTooLarge,
    #[error("response body exceeds configured bounds")]
    BodyTooLarge,
    #[error("cache object exceeds configured bounds")]
    ObjectTooLarge,
    #[error("surrogate tag is malformed or exceeds configured bounds")]
    InvalidTag,
    #[error("too many surrogate tags")]
    TooManyTags,
    #[error("304 response changes the representation Vary key")]
    VaryChanged,
    #[error("fill response does not belong to the fill's base key")]
    FillKeyMismatch,
    #[error("prepared entry belongs to a different cache storage identity")]
    PreparedEntryOwnerMismatch,
    #[error("fill key is not a bounded GET representation key")]
    InvalidFillKey,
    #[error("304 metadata cannot be merged")]
    InvalidNotModified,
}
