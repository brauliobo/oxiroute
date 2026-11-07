use std::{
    collections::HashMap,
    fmt::{Display, Formatter},
    str::FromStr,
    sync::{Arc, Mutex, MutexGuard},
};

use uuid::Uuid;

pub use crate::session_control::SessionId;

use crate::{
    RecorderEnqueueResult, RecorderNotification, RecorderWorkerPhase, RecorderWorkerStatus,
    live::VideoCodecIdentifier,
    recording::runtime::{
        RecorderCommandContext, RecorderController, RecorderReaper, RecorderReaperHandle,
        RecorderReaperOwner, RecorderRuntimeStatus, RecorderStartFailure, recorder_error_code,
    },
    relay::{RtmpRelayController, RtmpRelayStatus},
    session_control::{
        MAX_RTMP_SESSION_CONTROLS, RtmpClientSnapshot, RtmpSessionControl,
        RtmpSessionControlAction, RtmpSessionControlError, RtmpSessionControlOutcome,
    },
};

mod ids;
mod recording;
mod registration;
mod registry;
mod snapshot;
mod state;

pub use ids::*;
#[allow(clippy::wildcard_imports)]
use recording::*;
pub use registration::*;
#[allow(clippy::wildcard_imports)]
use registry::*;
pub use snapshot::*;
pub use state::*;

pub const MAX_RTMP_APPLICATION_BYTES: usize = 128;
pub const MAX_RTMP_STREAM_NAME_BYTES: usize = 512;
pub const MAX_RTMP_QUERY_BYTES: usize = 1024;

#[cfg(test)]
mod tests;
