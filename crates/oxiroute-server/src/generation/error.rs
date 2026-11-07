#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GenerationStatus {
    pub build_version: &'static str,
    pub disk_revision: Option<AuthoredRevision>,
    pub candidate_revision: Option<EffectiveRevision>,
    pub active_revision: Option<EffectiveRevision>,
    pub previous_revision: Option<EffectiveRevision>,
    pub quarantined_revision: Option<EffectiveRevision>,
    /// Number of the live generation: the activation count, or the master's generation id.
    pub generation: u64,
    pub active_accepting: bool,
    pub degraded: bool,
    pub last_failure: Option<&'static str>,
    pub last_rejection: Option<ConfigRejection>,
    pub prepares: u64,
    pub activations: u64,
    pub config_rejections: u64,
    pub failures: u64,
    pub rollbacks: u64,
    /// Replaced generations still draining, oldest first.
    pub retired_generations: Vec<RetiredGenerationStatus>,
    pub max_retired_generations: usize,
    pub pending_apply: Option<PendingApply>,
}

impl GenerationStatus {
    /// Whether the active generation can take traffic.
    ///
    /// A refused configuration file leaves the active generation serving, so it does not count.
    #[must_use]
    pub const fn serving(&self) -> bool {
        self.active_revision.is_some() && self.last_failure.is_none()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GenerationError {
    #[error("candidate runtime preparation failed: {0}")]
    Plan(#[source] Box<crate::ServicePlanError>),
    #[error("candidate runtime preparation failed")]
    RuntimePrepare,
    #[error("candidate preparation exceeded its deadline")]
    PreparationTimedOut,
    #[error("candidate listener reservation failed")]
    Listener(#[from] io::Error),
    #[error("candidate metrics preparation failed")]
    Metrics(#[from] crate::MetricsError),
    #[error("candidate RTMP preparation failed")]
    Rtmp(#[from] crate::ServicePlanError),
    #[error("candidate management token preparation failed")]
    ManagementToken,
    #[error("no prepared candidate is available")]
    NoCandidate,
    #[error("the prepared candidate was superseded")]
    CandidateSuperseded,
    #[error("an active-generation mutation is in progress")]
    MutationInProgress,
    #[error("active generation acceptors did not quiesce")]
    AcceptorQuiesce,
    #[error("no active generation is available")]
    NoActive,
    #[error("the active generation revision changed")]
    RevisionConflict,
    #[error("no previous generation is available")]
    NoPrevious,
    #[error("the previous generation revision is quarantined")]
    QuarantinedRevision,
}

impl From<RuntimeAcquisitionError> for GenerationError {
    fn from(error: RuntimeAcquisitionError) -> Self {
        match error {
            RuntimeAcquisitionError::ServicePlan(error) => Self::Plan(Box::new(error)),
            RuntimeAcquisitionError::PreparationTimedOut => Self::PreparationTimedOut,
        }
    }
}

pub(super) fn map_rtmp_runtime_set_error(error: RtmpRuntimeSetError) -> GenerationError {
    let timed_out = matches!(
        &error,
        RtmpRuntimeSetError::PreparationTimedOut { .. } | RtmpRuntimeSetError::StartTimedOut { .. }
    );
    drop(error);
    if timed_out {
        GenerationError::PreparationTimedOut
    } else {
        GenerationError::RuntimePrepare
    }
}

impl GenerationError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Plan(_) => "service_plan_prepare",
            Self::RuntimePrepare => "runtime_prepare",
            Self::PreparationTimedOut => "generation_prepare_timeout",
            Self::Listener(_) => "listener_reservation",
            Self::Metrics(_) => "metrics_prepare",
            Self::Rtmp(_) => "rtmp_prepare",
            Self::ManagementToken => "management_token_prepare",
            Self::NoCandidate => "candidate_missing",
            Self::CandidateSuperseded => "candidate_superseded",
            Self::MutationInProgress => "mutation_in_progress",
            Self::AcceptorQuiesce => "acceptor_quiesce",
            Self::NoActive => "generation_unavailable",
            Self::RevisionConflict => "generation_conflict",
            Self::NoPrevious => "previous_missing",
            Self::QuarantinedRevision => "generation_quarantined",
        }
    }
}
