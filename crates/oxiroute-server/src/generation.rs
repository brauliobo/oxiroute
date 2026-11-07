use std::{
    io,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use oxiroute_config::ValidatedConfig;
use oxiroute_config_source::ConfigFormat;
use oxiroute_rtmp::{
    PreparedRtmpRuntimeSet, RtmpAutoPushStatus, RtmpControlHandle, RtmpPrepareContext,
    RtmpPrepareMode, RtmpRuntimeSetError, RtmpServiceHandle, RtmpShutdown,
};
#[cfg(target_os = "linux")]
use oxiroute_supervision_unix::DescriptorSet;
use pingora::apps::{AcceptGate, AcceptGateClose, AcceptOwnership};
use serde::Serialize;

use crate::generation_resources::GenerationResources;
use crate::listener_inventory::{ListenerId, ListenerInventory};
use crate::routing::ServerAdminState;
use crate::rtmp_generation_runtime::{RtmpRetirement, RtmpRetirementRegistry};
#[cfg(test)]
use crate::service_plan::acquire_runtime_services;
use crate::{
    ListenerMetrics, ListenerReservations, MetricsError, ProcessRuntime, RuntimeMetrics,
    RuntimePlan, ServiceKind,
    config_coordinator::{
        AuthoredRevision, ConfigRejection, EffectiveRevision, ResolvedConfigDocument,
    },
    runtime_plan,
    service_plan::validation_plan,
    service_plan::{
        RuntimeAcquisitionError, acquire_runtime_services_with_deadline, validate_runtime_services,
    },
};

pub(crate) const GENERATION_PREPARATION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RuntimeReferenceKind {
    ForwardHttp1,
    Http1,
    Http2,
    WebSocket,
    Tcp,
    Rtmp,
    Udp,
    ForwardHttp3,
    Http3,
}

impl RuntimeReferenceKind {
    const fn index(self) -> usize {
        match self {
            Self::ForwardHttp1 => 0,
            Self::Http1 => 1,
            Self::Http2 => 2,
            Self::WebSocket => 3,
            Self::Tcp => 4,
            Self::Rtmp => 5,
            Self::Udp => 6,
            Self::ForwardHttp3 => 7,
            Self::Http3 => 8,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationRevision {
    pub disk: AuthoredRevision,
    pub candidate: EffectiveRevision,
}

mod error;
mod handles;
mod manager;
mod prepared;
mod runtime;
mod transaction;

pub use error::*;
pub use handles::*;
pub use manager::*;
pub use prepared::*;
pub use runtime::*;
#[allow(clippy::wildcard_imports)]
use transaction::*;

#[cfg(test)]
pub(crate) mod tests;
