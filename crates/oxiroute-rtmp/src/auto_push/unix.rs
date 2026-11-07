use std::{
    collections::{BTreeMap, VecDeque},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::{
        fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::Duration,
};

use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::wire::{Reader, put_string};
use super::{RtmpAutoPushConfig, RtmpAutoPushError, RtmpAutoPushStatus};
use crate::{
    LiveHub, MediaEvent, MediaEventKind, PublisherIncarnation, PublisherRegistration, RtmpRegistry,
    SessionId, StreamKey, VideoCodec, clock::unix_time_ms,
    media_snapshot::MediaSnapshotAccumulator,
};

mod coordinator;
mod endpoint;
mod protocol;
mod queue;
mod shared;

pub(crate) use coordinator::*;
#[allow(clippy::wildcard_imports)]
use endpoint::*;
pub(super) use protocol::*;
#[allow(clippy::wildcard_imports)]
use queue::*;
#[allow(clippy::wildcard_imports)]
use shared::*;

const PROTOCOL_MAGIC: [u8; 4] = *b"ORAP";
const PROTOCOL_VERSION: u8 = 1;
const PACKET_HELLO: u8 = 1;
const PACKET_HELLO_ACK: u8 = 2;
const PACKET_MEDIA: u8 = 3;
const HELLO_NONCE_BYTES: usize = 16;
const SECRET_BYTES: usize = 32;
const MAX_SECRET_BYTES: usize = 128;
const MAX_SERVICE_ID_BYTES: usize = 256;
const MAX_SESSION_ID_BYTES: usize = 64;
const MAX_STREAM_COMPONENT_BYTES: usize = 512;
const MAX_PACKET_BYTES: usize = 16 * 1024 * 1024 + 256;
const SOCKET_MODE: u32 = 0o600;
const DIRECTORY_MODE: u32 = 0o700;
const SOCKET_PREFIX: &str = "oxir-";
const SOCKET_SUFFIX: &str = ".sock";
const SOCKET_WAIT: Duration = Duration::from_millis(100);
const DISCOVERY_LIMIT_MULTIPLIER: usize = 2;

type WorkerId = [u8; 16];
