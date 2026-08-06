use std::{
    cmp::Ordering,
    fmt,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
    },
};

use super::{ActiveCertificateGeneration, CertbotLineage, TlsBuildError};

const MAX_CAS_REREADS: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CertbotActivationDirection {
    Forward,
    Rollback,
    Replacement,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CertbotReconcileOutcome {
    Unchanged {
        archive_revision: u64,
    },
    Activated {
        previous_archive_revision: u64,
        archive_revision: u64,
        direction: CertbotActivationDirection,
    },
}

impl CertbotReconcileOutcome {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unchanged { .. } => "unchanged",
            Self::Activated {
                direction: CertbotActivationDirection::Forward,
                ..
            } => "activated_forward",
            Self::Activated {
                direction: CertbotActivationDirection::Rollback,
                ..
            } => "activated_rollback",
            Self::Activated {
                direction: CertbotActivationDirection::Replacement,
                ..
            } => "activated_replacement",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CertbotReconcileError {
    #[error(
        "Certbot candidate for `{certificate}` was rejected; active archive revision {active_archive_revision} was retained"
    )]
    InvalidCandidate {
        certificate: String,
        active_archive_revision: u64,
        #[source]
        source: Box<TlsBuildError>,
    },
    #[error(
        "Certbot candidate for `{certificate}` was reread after {attempts} publication conflicts"
    )]
    PublicationConflict {
        certificate: String,
        active_archive_revision: u64,
        attempts: usize,
    },
}

impl CertbotReconcileError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidCandidate { .. } => "invalid_candidate",
            Self::PublicationConflict { .. } => "publication_conflict",
        }
    }
}

struct ReconcileState {
    active_archive_revision: u64,
    last_outcome: Option<&'static str>,
    last_error_code: Option<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertbotReconcilerStatus {
    pub certificate: String,
    pub active_archive_revision: u64,
    pub active_content_revision: String,
    pub not_after: String,
    pub last_outcome: Option<&'static str>,
    pub last_error_code: Option<&'static str>,
}

pub struct CertbotReconciler {
    lineage: CertbotLineage,
    certificate: String,
    declared_dns_names: Vec<String>,
    active: Arc<ActiveCertificateGeneration>,
    state: Mutex<ReconcileState>,
}

impl CertbotReconciler {
    #[must_use]
    pub fn new(
        lineage: CertbotLineage,
        certificate: impl Into<String>,
        declared_dns_names: Vec<String>,
        active_archive_revision: u64,
        active: Arc<ActiveCertificateGeneration>,
    ) -> Self {
        Self {
            lineage,
            certificate: certificate.into(),
            declared_dns_names,
            active,
            state: Mutex::new(ReconcileState {
                active_archive_revision,
                last_outcome: None,
                last_error_code: None,
            }),
        }
    }

    #[must_use]
    pub fn active_archive_revision(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .active_archive_revision
    }

    #[must_use]
    pub const fn lineage(&self) -> &CertbotLineage {
        &self.lineage
    }

    #[must_use]
    pub const fn active_generation(&self) -> &Arc<ActiveCertificateGeneration> {
        &self.active
    }

    #[must_use]
    pub fn status(&self) -> CertbotReconcilerStatus {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let generation = self.active.snapshot();
        CertbotReconcilerStatus {
            certificate: self.certificate.clone(),
            active_archive_revision: state.active_archive_revision,
            active_content_revision: generation.metadata().revision.clone(),
            not_after: generation.metadata().validity.not_after.clone(),
            last_outcome: state.last_outcome,
            last_error_code: state.last_error_code,
        }
    }

    /// Reconciles the current complete Certbot revision against the active generation.
    ///
    /// # Errors
    ///
    /// Returns a redacted error after retaining the active generation when the candidate is
    /// invalid or repeated compare-and-swap conflicts do not settle.
    pub fn reconcile(&self) -> Result<CertbotReconcileOutcome, CertbotReconcileError> {
        self.reconcile_recorded(None, &mut |_| {})
            .map(Option::unwrap)
    }

    #[cfg(test)]
    /// Reconciles while invoking a deterministic test hook before each publication attempt.
    ///
    /// # Errors
    ///
    /// Returns the same redacted candidate or publication errors as [`Self::reconcile`].
    pub fn reconcile_with_before_publish(
        &self,
        mut before_publish: impl FnMut(usize),
    ) -> Result<CertbotReconcileOutcome, CertbotReconcileError> {
        self.reconcile_recorded(None, &mut before_publish)
            .map(Option::unwrap)
    }

    pub(crate) fn reconcile_while_running(
        &self,
        gate: &PublicationGate,
    ) -> Result<Option<CertbotReconcileOutcome>, CertbotReconcileError> {
        self.reconcile_recorded(Some(gate), &mut |_| {})
    }

    fn reconcile_recorded(
        &self,
        gate: Option<&PublicationGate>,
        before_publish: &mut dyn FnMut(usize),
    ) -> Result<Option<CertbotReconcileOutcome>, CertbotReconcileError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = self.reconcile_inner(&mut state, gate, before_publish);
        match &result {
            Ok(Some(outcome)) => {
                state.last_outcome = Some(outcome.code());
                state.last_error_code = None;
            }
            Err(error) => {
                state.last_outcome = None;
                state.last_error_code = Some(error.code());
            }
            Ok(None) => {}
        }
        result
    }

    fn reconcile_inner(
        &self,
        state: &mut ReconcileState,
        gate: Option<&PublicationGate>,
        before_publish: &mut dyn FnMut(usize),
    ) -> Result<Option<CertbotReconcileOutcome>, CertbotReconcileError> {
        for attempt in 0..MAX_CAS_REREADS {
            if gate.is_some_and(PublicationGate::is_stopped) {
                return Ok(None);
            }
            let candidate = self
                .lineage
                .load_candidate(&self.certificate, &self.declared_dns_names)
                .map_err(|source| CertbotReconcileError::InvalidCandidate {
                    certificate: self.certificate.clone(),
                    active_archive_revision: state.active_archive_revision,
                    source: Box::new(source),
                })?;
            let archive_revision = candidate.archive_revision();
            let expected = self.active.snapshot();
            if candidate.generation().metadata().revision == expected.metadata().revision {
                before_publish(attempt);
                let publication = if let Some(gate) = gate {
                    let Some(publication) = gate.publish(|| {
                        self.active
                            .publish_prevalidated_if_current(&expected, Arc::clone(&expected))
                    }) else {
                        return Ok(None);
                    };
                    publication
                } else {
                    self.active
                        .publish_prevalidated_if_current(&expected, Arc::clone(&expected))
                };
                if publication {
                    state.active_archive_revision = archive_revision;
                    return Ok(Some(CertbotReconcileOutcome::Unchanged {
                        archive_revision,
                    }));
                }
                continue;
            }
            let previous_archive_revision = state.active_archive_revision;
            let direction = match archive_revision.cmp(&previous_archive_revision) {
                Ordering::Greater => CertbotActivationDirection::Forward,
                Ordering::Less => CertbotActivationDirection::Rollback,
                Ordering::Equal => CertbotActivationDirection::Replacement,
            };
            before_publish(attempt);
            let replacement = Arc::new(candidate.into_generation());
            let publication = if let Some(gate) = gate {
                let Some(publication) = gate.publish(|| {
                    self.active
                        .publish_prevalidated_if_current(&expected, Arc::clone(&replacement))
                }) else {
                    return Ok(None);
                };
                publication
            } else {
                self.active
                    .publish_prevalidated_if_current(&expected, replacement)
            };
            if publication {
                state.active_archive_revision = archive_revision;
                return Ok(Some(CertbotReconcileOutcome::Activated {
                    previous_archive_revision,
                    archive_revision,
                    direction,
                }));
            }
        }

        Err(CertbotReconcileError::PublicationConflict {
            certificate: self.certificate.clone(),
            active_archive_revision: state.active_archive_revision,
            attempts: MAX_CAS_REREADS,
        })
    }
}

impl fmt::Debug for CertbotReconciler {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CertbotReconciler")
            .field("certificate", &self.certificate)
            .field("active_archive_revision", &self.active_archive_revision())
            .finish_non_exhaustive()
    }
}

pub(crate) struct PublicationGate {
    stopped: AtomicBool,
    publication: Mutex<()>,
}

impl PublicationGate {
    pub(crate) fn new() -> Self {
        Self {
            stopped: AtomicBool::new(false),
            publication: Mutex::new(()),
        }
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(AtomicOrdering::Acquire)
    }

    pub(crate) fn stop(&self) {
        let _publication = self
            .publication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.stopped.store(true, AtomicOrdering::Release);
    }

    pub(crate) fn publish<T>(&self, publish: impl FnOnce() -> T) -> Option<T> {
        let _publication = self
            .publication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.is_stopped() {
            None
        } else {
            Some(publish())
        }
    }
}
