//! Bounded shared HTTP cache policy, memory and persistent storage, and collapsed-forwarding
//! primitives.

mod cache;
mod clock;
mod disk;
mod http;
mod key;
mod policy;

pub use cache::{
    Cache, CacheConfig, CacheError, CacheResponse, CacheStats, CachedResponse, FillGuard, FillJoin,
    FillOutcome, FillWaiter, Lookup, LookupStatus, PreparedEntry, PurgeResult, StoreOutcome,
};
pub use clock::{Clock, MonoTime, SystemClock};
pub use disk::{
    DiskCache, DiskCacheConfig, DiskCacheError, DiskCacheStats, DiskFillGuard, DiskFillJoin,
    DiskQuotaScope,
};
pub use key::{BaseKey, CacheKey, KeyError, RequestKeyInput, Vary};
pub use policy::{
    CacheControl, CacheTimeline, CacheTimelineError, ParseError, RequestMode, RequestPolicy,
    ResponseRejection, ResponseTiming, RevalidationError, Validators, current_age,
    merge_not_modified_headers,
};
