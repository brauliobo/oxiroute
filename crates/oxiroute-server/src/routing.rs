use std::{
    error::Error,
    fmt,
    hash::{BuildHasher as _, RandomState},
    io,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, RwLock,
        atomic::{
            AtomicBool, AtomicU8, AtomicU16, AtomicU32, AtomicU64, AtomicUsize, Ordering, fence,
        },
    },
    time::{Duration, SystemTime},
};

use crate::{
    health::ReadmissionProbe,
    observability::upstream_stats::{ServerStats, upstream_stats},
};
use arc_swap::ArcSwap;
use http::{
    HeaderMap, Method, Uri,
    uri::{Authority, PathAndQuery},
};
use oxiroute_config::{
    CircuitBreaker, HealthStartup, HttpHostSelector, HttpPathSelector, OutlierDetection,
    PassiveHealthPolicy as ConfigPassiveHealthPolicy, PassiveObserve, PassiveOnError,
    PoolQueuePolicy, PoolResilience, Readmission, UpstreamAlgorithm, UpstreamEndpoint,
    canonicalize_http_path,
};
use serde::{Deserialize, Serialize, Serializer};
use tokio::{
    sync::Notify,
    time::{Instant, timeout_at},
};

mod breaker;
mod canary;
mod endpoint;
mod health;
mod lease;
mod load_index;
mod names;
mod outlier;
mod policy;
mod pool;
mod queue;
mod queue_wait;
mod rendezvous;
mod resolved;
mod route;
mod schedule;
mod selectable;
mod server;
mod weight_index;

pub use breaker::CircuitBreakerPolicy;
#[allow(clippy::wildcard_imports)]
use breaker::{Breaker, BreakerTransition};
pub use canary::{CanaryResult, CanaryRun, CanarySelection, CanarySnapshot, CanaryState};
pub use endpoint::*;
pub use health::*;
pub use lease::*;
use load_index::{LoadIndex, LoadLog};
use names::NameIndex;
use outlier::OutlierDetector;
pub use outlier::OutlierPolicy;
pub use policy::*;
#[allow(clippy::wildcard_imports)]
use queue::*;
use queue_wait::{WaitLedger, WaitTicket};
use resolved::ResolvedAddresses;
pub use route::*;
pub use schedule::QueueTag;
use schedule::{QueueSchedule, WaitQueue};
use selectable::SelectableView;
pub use server::ServerOrigin;
pub(crate) use server::*;
use weight_index::WeightIndex;

#[cfg(test)]
mod membership_tests;
#[cfg(test)]
mod tests;
