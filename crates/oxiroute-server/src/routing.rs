use std::{
    collections::VecDeque,
    error::Error,
    fmt,
    hash::{BuildHasher as _, RandomState},
    io,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{AtomicBool, AtomicU8, AtomicU16, AtomicU64, Ordering, fence},
    },
    time::{Duration, SystemTime},
};

use crate::health::ReadmissionProbe;
use http::{
    Method, Uri,
    uri::{Authority, PathAndQuery},
};
use oxiroute_config::{
    CircuitBreaker, HealthStartup, HttpHostSelector, HttpPathSelector,
    PassiveHealthPolicy as ConfigPassiveHealthPolicy, PassiveObserve, PassiveOnError,
    PoolResilience, Readmission, UpstreamAlgorithm, UpstreamEndpoint, canonicalize_http_path,
};
use serde::{Deserialize, Serialize, Serializer};
use tokio::{
    sync::Notify,
    time::{Instant, timeout_at},
};

mod breaker;
mod endpoint;
mod health;
mod lease;
mod policy;
mod pool;
mod queue;
mod queue_wait;
mod route;
mod server;

pub use breaker::CircuitBreakerPolicy;
#[allow(clippy::wildcard_imports)]
use breaker::{Breaker, BreakerTransition};
pub use endpoint::*;
pub use health::*;
pub use lease::*;
pub use policy::*;
#[allow(clippy::wildcard_imports)]
use queue::*;
use queue_wait::{WaitLedger, WaitTicket};
pub use route::*;
pub(crate) use server::*;

#[cfg(test)]
mod tests;
