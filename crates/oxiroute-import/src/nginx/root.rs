use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use oxiroute_config::{ConfigDraft, RtmpRecorderTimezone, UpstreamTls};

use crate::{
    ActivationRequirement, ActivationRequirementKind, CanonicalCandidate, CanonicalProvenance,
    DeploymentRequirement, DeploymentRequirementKind, Diagnostic, DiagnosticStage,
    E_DUPLICATE_IDENTITY, E_INVALID_VALUE, E_UNSUPPORTED_FEATURE, OperationalOverlayKind,
    OperationalOverlayRequirement, Report, Severity, SourceImportMetadata,
    candidate::{CanonicalCandidateState, finalize_candidate},
};

use super::{
    BlockedRtmpService, BlockedService, BlockedStreamService, DirectiveOrigin, ExpandedDirective,
    NginxValue, OccurrenceDecision, OccurrenceDisposition, OccurrenceId, Provenance, SourceGraph,
    Word, load, semantic::bearer_gate_condition,
};

mod import;
mod model;
mod overlays;
mod scan;

pub use import::*;
pub use model::*;
use overlays::{
    append_default_access_log_overlay, append_default_error_page_overlay,
    append_host_timezone_overlays, append_recording_root_overlay, append_supplied_overlays,
    reject_duplicate_overlay_keys, reject_unsatisfied_overlays,
};
use scan::scan_root;
