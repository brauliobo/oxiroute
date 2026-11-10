use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard, Weak},
    time::Duration,
};

use bytes::Bytes;
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use tokio::sync::watch;

use crate::{
    BaseKey, CacheKey, CacheTimeline, Clock, KeyError, MonoTime, RequestKeyInput,
    ResponseRejection, ResponseTiming, SystemClock, Validators,
    policy::{
        ParseError, RequestMode, RequestPolicy, ResponsePolicy, ResponsePolicyInput,
        merge_not_modified_headers,
    },
};

const ENTRY_ACCOUNTING_OVERHEAD: usize = 128;
const HEADER_ACCOUNTING_OVERHEAD: usize = 32;
const TAG_ACCOUNTING_OVERHEAD: usize = 16;

mod config;
mod entries;
mod flight;
mod store;
mod types;

pub use config::*;
use entries::Entries;
#[allow(clippy::wildcard_imports)]
use flight::*;
pub use store::*;
pub use types::*;

#[cfg(test)]
mod tests;
