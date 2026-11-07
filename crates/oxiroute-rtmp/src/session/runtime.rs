use std::{
    collections::BTreeMap,
    net::IpAddr,
    sync::{
        Arc, Mutex, Once, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use crate::{
    CatalogError, LiveHub, LiveHubError, MediaApplication, PublisherLease, RecorderDefinition,
    RtmpAutoPushConfig, RtmpAutoPushConfigError, RtmpAutoPushError, RtmpAutoPushStatus,
    RtmpCallbackPolicy, RtmpPullTarget, RtmpPushTarget, RtmpRecorderPolicy, RtmpRecorderStart,
    RtmpRegistry, SessionId, StreamKey, VodApplication, VodError,
    auto_push::AutoPushCoordinator,
    exec::ExecProfile,
    exec::worker::ExecProfileSet,
    recording::runtime::{
        RecorderController, RecorderReaperHandle, RecorderReaperOwner, RecorderShutdownControl,
        RtmpRecorderShutdown,
    },
    relay::{RtmpPullController, RtmpRelayController},
};
use rml_rtmp::messages::Amf0Limits;

use super::{
    MAX_INBOUND_AMF0_CONTAINER_ENTRIES, MAX_INBOUND_AMF0_DEPTH, MAX_INBOUND_AMF0_STRING_BYTES,
    MAX_INBOUND_AMF0_VALUES, MAX_INBOUND_CHUNK_SIZE, MAX_INBOUND_MESSAGE_SIZE, RtmpSession,
    playback::PlaybackSession,
    publish::{PublishSession, PublisherOutputs},
    vod_playback::{VodPlaybackSession, VodPlaybackStart},
};

mod application;
mod policy;
mod service;

pub use application::*;
pub use policy::*;
pub use service::*;

pub const RTMP_STALE_PUBLISHER_THRESHOLD_MS: u64 = 30_000;
const MAX_RTMP_TOKEN_BYTES: usize = 128;
const DEFAULT_RTMP_APPLICATION_CONNECTIONS: usize = 1_024;
const DEFAULT_RTMP_APPLICATION_PUBLISHERS: usize = 256;
const DEFAULT_RTMP_APPLICATION_VIEWERS: usize = 1_024;
const DEFAULT_RTMP_ACK_WINDOW_SIZE: u32 = 5_000_000;

#[cfg(test)]
std::thread_local! {
    static PREPARATION_STARTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static REAPER_STARTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static PULL_STARTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests;
