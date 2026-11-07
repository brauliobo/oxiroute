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
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering, fence},
    },
    time::{Duration, SystemTime},
};

use http::{
    Method, Uri,
    uri::{Authority, PathAndQuery},
};
use oxiroute_config::{
    HealthStartup, HttpHostSelector, HttpPathSelector,
    PassiveHealthPolicy as ConfigPassiveHealthPolicy, PassiveObserve, PassiveOnError,
    PoolResilience, UpstreamAlgorithm, UpstreamEndpoint, canonicalize_http_path,
};
use serde::{Deserialize, Serialize, Serializer};
use tokio::{
    sync::Notify,
    time::{Instant, timeout_at},
};

mod endpoint;
mod health;
mod lease;
mod policy;
mod pool;
mod queue;
mod route;
mod server;

pub use endpoint::*;
pub use health::*;
pub use lease::*;
pub use policy::*;
#[allow(clippy::wildcard_imports)]
use queue::*;
pub use route::*;
pub(crate) use server::*;

#[cfg(test)]
mod tests;
