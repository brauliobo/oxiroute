use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread,
    time::Duration,
};

use openssl::{rand::rand_bytes, symm};

use crate::{
    MediaEvent, MediaEventKind, PublisherIncarnation, StreamKey,
    clock::unix_time_ms,
    media::dash::{DashOutputConfig, DashSegmenter},
    media::parser::{parse_aac_configuration, parse_avc_configuration},
    media::segment_window::SegmentWindowConfig,
    media::storage::{MediaStore, MediaStoreError},
};

mod config;
mod hls;
mod paths;
mod publisher;
mod ts;

pub use config::*;
#[allow(clippy::wildcard_imports)]
use hls::*;
#[allow(clippy::wildcard_imports)]
use paths::*;
pub use publisher::*;
#[allow(clippy::wildcard_imports)]
use ts::*;

const TS_PACKET_BYTES: usize = 188;
const VIDEO_PID: u16 = 0x0101;
const AUDIO_PID: u16 = 0x0102;
const PMT_PID: u16 = 0x1000;
const MAX_PLAYLIST_SEGMENTS: usize = 512;
const MAX_HLS_VARIANT_NAME_BYTES: usize = 128;
const MAX_HLS_KEY_URL_PREFIX_BYTES: usize = 512;

#[cfg(feature = "fuzzing")]
pub(crate) fn fuzz_media_configuration(payload: &[u8]) {
    let _ = parse_avc_config(payload);
    let _ = parse_aac_config(payload);
}

#[cfg(test)]
mod tests;
