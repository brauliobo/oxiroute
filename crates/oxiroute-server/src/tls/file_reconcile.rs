use std::{
    fmt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use super::{
    ActiveCertificateGeneration, CertificateGeneration, TlsBuildError,
    certificate::{CertificatePublication, CertificatePublicationError, PublicationGate},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileReconcileOutcome {
    Unchanged,
    Activated,
}

impl FileReconcileOutcome {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Unchanged => "unchanged",
            Self::Activated => "activated",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FileReconcileError {
    #[error(
        "direct-file candidate for `{certificate}` was rejected; active content revision {active_content_revision} was retained"
    )]
    InvalidCandidate {
        certificate: String,
        active_content_revision: String,
        #[source]
        source: Box<TlsBuildError>,
    },
    #[error(
        "direct-file candidate for `{certificate}` was reread after {attempts} publication conflicts"
    )]
    PublicationConflict {
        certificate: String,
        active_content_revision: String,
        attempts: usize,
    },
}

impl FileReconcileError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidCandidate { .. } => "invalid_candidate",
            Self::PublicationConflict { .. } => "publication_conflict",
        }
    }
}

struct ReconcileState {
    last_outcome: Option<&'static str>,
    last_error_code: Option<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FileReconcilerStatus {
    pub certificate: String,
    pub active_content_revision: String,
    pub not_after: String,
    pub last_outcome: Option<&'static str>,
    pub last_error_code: Option<&'static str>,
}

pub struct FileReconciler {
    certificate: String,
    declared_dns_names: Vec<String>,
    certificate_chain_path: PathBuf,
    private_key_path: PathBuf,
    active: Arc<ActiveCertificateGeneration>,
    state: Mutex<ReconcileState>,
}

impl FileReconciler {
    #[must_use]
    pub fn new(
        certificate: impl Into<String>,
        declared_dns_names: Vec<String>,
        certificate_chain_path: impl Into<PathBuf>,
        private_key_path: impl Into<PathBuf>,
        active: Arc<ActiveCertificateGeneration>,
    ) -> Self {
        Self {
            certificate: certificate.into(),
            declared_dns_names,
            certificate_chain_path: certificate_chain_path.into(),
            private_key_path: private_key_path.into(),
            active,
            state: Mutex::new(ReconcileState {
                last_outcome: None,
                last_error_code: None,
            }),
        }
    }

    #[must_use]
    pub fn certificate_chain_path(&self) -> &Path {
        self.certificate_chain_path.as_path()
    }

    #[must_use]
    pub fn private_key_path(&self) -> &Path {
        self.private_key_path.as_path()
    }

    #[must_use]
    pub const fn active_generation(&self) -> &Arc<ActiveCertificateGeneration> {
        &self.active
    }

    #[must_use]
    pub fn status(&self) -> FileReconcilerStatus {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let generation = self.active.snapshot();
        FileReconcilerStatus {
            certificate: self.certificate.clone(),
            active_content_revision: generation.metadata().revision.clone(),
            not_after: generation.metadata().validity.not_after.clone(),
            last_outcome: state.last_outcome,
            last_error_code: state.last_error_code,
        }
    }

    /// Reconciles one stable, complete direct-file pair against the active generation.
    ///
    /// # Errors
    ///
    /// Returns a redacted error after retaining the active generation when either file is missing,
    /// unstable, malformed, mismatched, or otherwise fails TLS validation.
    pub fn reconcile(&self) -> Result<FileReconcileOutcome, FileReconcileError> {
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
    ) -> Result<FileReconcileOutcome, FileReconcileError> {
        self.reconcile_recorded(None, &mut before_publish)
            .map(Option::unwrap)
    }

    pub(crate) fn reconcile_while_running(
        &self,
        gate: &PublicationGate,
    ) -> Result<Option<FileReconcileOutcome>, FileReconcileError> {
        self.reconcile_recorded(Some(gate), &mut |_| {})
    }

    fn reconcile_recorded(
        &self,
        gate: Option<&PublicationGate>,
        before_publish: &mut dyn FnMut(usize),
    ) -> Result<Option<FileReconcileOutcome>, FileReconcileError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let result = self.reconcile_inner(gate, before_publish);
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
        gate: Option<&PublicationGate>,
        before_publish: &mut dyn FnMut(usize),
    ) -> Result<Option<FileReconcileOutcome>, FileReconcileError> {
        let publication = self.active.publish_transaction(gate, before_publish, || {
            CertificateGeneration::from_files(
                self.certificate.clone(),
                &self.declared_dns_names,
                &self.certificate_chain_path,
                &self.private_key_path,
            )
            .map(|candidate| ((), candidate))
            .map_err(|source| FileReconcileError::InvalidCandidate {
                certificate: self.certificate.clone(),
                active_content_revision: self.active.snapshot().metadata().revision.clone(),
                source: Box::new(source),
            })
        });
        match publication {
            Ok(CertificatePublication::Unchanged(())) => Ok(Some(FileReconcileOutcome::Unchanged)),
            Ok(CertificatePublication::Activated(())) => Ok(Some(FileReconcileOutcome::Activated)),
            Err(CertificatePublicationError::Candidate(error)) => Err(error),
            Err(CertificatePublicationError::Conflict { attempts }) => {
                Err(FileReconcileError::PublicationConflict {
                    certificate: self.certificate.clone(),
                    active_content_revision: self.active.snapshot().metadata().revision.clone(),
                    attempts,
                })
            }
            Err(CertificatePublicationError::Stopped) => Ok(None),
        }
    }
}

impl fmt::Debug for FileReconciler {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FileReconciler")
            .field("certificate", &self.certificate)
            .field(
                "active_content_revision",
                &self.active.snapshot().metadata().revision,
            )
            .finish_non_exhaustive()
    }
}
