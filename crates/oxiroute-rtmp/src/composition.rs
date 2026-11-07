#![allow(clippy::missing_errors_doc)]

use std::{
    fmt,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use crate::{
    DashOutputConfig, DashSegmentNaming, DestinationPolicyError, ExecEnvironment,
    ExecFilesystemPolicy, ExecLimits, ExecMode, ExecNetworkPolicy, ExecProfile, ExecProfileError,
    ExecTrigger, HlsFragmentNaming, HlsKeyConfig, HlsOutputConfig, HlsValueError, HlsVariant,
    LiveHub, LiveHubLimits, MediaApplication, MediaStore, MediaStoreLimits, RecorderWorkerConfig,
    RecorderWorkerStartError, RecordingPathPolicy, RecordingStoreLimits, RecordingStoreLimitsError,
    RtmpAccessAction, RtmpAccessPolicy, RtmpAccessRule, RtmpApplication, RtmpAutoPushConfig,
    RtmpAutoPushConfigError, RtmpCallbackMethod, RtmpCallbackPolicy, RtmpCallbackValueError,
    RtmpNetwork, RtmpOutboundPolicy, RtmpPullTarget, RtmpPushApplication, RtmpPushTarget,
    RtmpRecorderPolicy, RtmpRecorderStart, RtmpSessionCeilings, RtmpSessionLimitError,
    RtmpSessionLimits, RtmpStreamPath, RtmpTokenPolicy, RtmpTransport, VodApplication, VodLimits,
    VodSourceDefinition, VodValueError, validate_callback_url_intrinsic,
};

mod access;
mod error;
mod media;
mod plans;
mod relay;
mod service;
mod validate;

pub use access::*;
pub use error::*;
pub use media::*;
pub use plans::*;
pub use relay::*;
pub use service::*;
#[allow(clippy::wildcard_imports)]
use validate::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RtmpPrepareMode {
    Activation,
    Validation,
}
