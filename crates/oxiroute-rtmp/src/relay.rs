use std::{
    collections::{BTreeMap, VecDeque},
    fmt,
    io::{self, Read, Write},
    net::{SocketAddr, ToSocketAddrs},
    sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock},
    time::{Duration, Instant},
};

mod config;
mod executor;
mod pull;
mod push;
mod resolver;

pub use config::*;
pub(crate) use pull::*;
pub(crate) use push::*;
pub use resolver::*;

use executor::BoundedExecutor;

use bytes::Bytes;
use rml_rtmp::{
    handshake::{Handshake, HandshakeProcessResult, PeerType},
    sessions::{
        ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult,
        PublishRequestType,
    },
    time::RtmpTimestamp,
};

use crate::{
    LiveHub, MediaEvent, MediaEventKind, RtmpClientOptions, RtmpOutboundPolicy, RtmpRegistry,
    RtmpTransport, SessionId, StreamKey, VideoCodec,
    client::{self, RtmpStream},
    clock::unix_time_ms,
    media_snapshot::MediaSnapshotAccumulator,
};

const HANDSHAKE_RESPONSE_BYTES: usize = 3_073;
const RELAY_READ_BUFFER_BYTES: usize = 16 * 1_024;
const RELAY_QUEUE_POLL: Duration = Duration::from_millis(20);
pub const RTMP_RELAY_WORKER_THREADS: usize = 8;
pub const MAX_QUEUED_RTMP_RELAYS: usize = 64;
const MAX_RTMP_DESTINATION_ADDRESSES: usize = 32;

#[cfg(test)]
mod tests;
