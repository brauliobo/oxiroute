use std::{
    cmp::Ordering,
    fmt,
    sync::{Arc, Mutex},
};

use super::{
    ActiveCertificateGeneration, CertbotLineage, TlsBuildError,
    certificate::{CertificatePublication, CertificatePublicationError, PublicationGate},
};

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
        let publication = self.active.publish_transaction(gate, before_publish, || {
            let candidate = self
                .lineage
                .load_candidate(&self.certificate, &self.declared_dns_names)
                .map_err(|source| CertbotReconcileError::InvalidCandidate {
                    certificate: self.certificate.clone(),
                    active_archive_revision: state.active_archive_revision,
                    source: Box::new(source),
                })?;
            Ok((candidate.archive_revision(), candidate.into_generation()))
        });
        match publication {
            Ok(CertificatePublication::Unchanged(archive_revision)) => {
                state.active_archive_revision = archive_revision;
                Ok(Some(CertbotReconcileOutcome::Unchanged {
                    archive_revision,
                }))
            }
            Ok(CertificatePublication::Activated(archive_revision)) => {
                let previous_archive_revision = state.active_archive_revision;
                let direction = match archive_revision.cmp(&previous_archive_revision) {
                    Ordering::Greater => CertbotActivationDirection::Forward,
                    Ordering::Less => CertbotActivationDirection::Rollback,
                    Ordering::Equal => CertbotActivationDirection::Replacement,
                };
                state.active_archive_revision = archive_revision;
                Ok(Some(CertbotReconcileOutcome::Activated {
                    previous_archive_revision,
                    archive_revision,
                    direction,
                }))
            }
            Err(CertificatePublicationError::Candidate(error)) => Err(error),
            Err(CertificatePublicationError::Conflict { attempts }) => {
                Err(CertbotReconcileError::PublicationConflict {
                    certificate: self.certificate.clone(),
                    active_archive_revision: state.active_archive_revision,
                    attempts,
                })
            }
            Err(CertificatePublicationError::Stopped) => Ok(None),
        }
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
