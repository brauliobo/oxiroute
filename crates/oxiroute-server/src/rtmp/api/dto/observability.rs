use oxiroute_rtmp::{RecorderPhase, RtmpCatalogSnapshot};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::management::{ListenerDto, PoolDto};
use super::{DecimalCounter, management::LatencyDto};
use crate::observability::operational_event::{AuditComponentState, AuditStatus};
use crate::{
    AccessRecord, AcmeManagedCertificateSnapshot, AdministrativeState, CertbotCertificateSnapshot,
    CertbotWatcherHealth, CertbotWatcherSnapshot, ComponentState, ComponentStatus,
    DirectFileCertificateSnapshot, DirectFileWatcherSnapshot, GenerationStatus, HostSnapshot,
    ListenerRuntimeState, ListenerSnapshot, ObservedTransport, ProcessSnapshot, RuntimeMode,
    RuntimeSnapshot, TopologyNodeKind, TopologySnapshot, TrafficSnapshot,
    TransportOperationCountSnapshot, TransportOperationSnapshot, TransportOutcome,
};

mod certificates;
mod enums;
mod monitoring;
mod requests;
mod status;

use certificates::{
    AcmeManagedCertificateDto, CertbotCertificateDto, CertificateWatcherDto,
    DirectFileCertificateDto,
};
use enums::{
    AdministrativeStateDto, AuditComponentStateDto, CertbotWatcherHealthDto, ComponentStateDto,
    ObservedTransportDto, RecorderPhaseDto, RelayDnsRefreshFailureDto, RelayFailureDto,
    RelayPhaseDto, TransportOutcomeDto,
};
pub(crate) use monitoring::*;
pub(crate) use requests::RequestListResponse;
pub(crate) use status::*;

#[cfg(test)]
mod tests;
