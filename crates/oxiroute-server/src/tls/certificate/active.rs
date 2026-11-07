#[allow(clippy::wildcard_imports)]
use super::*;

pub struct ActiveCertificateGeneration {
    name: String,
    dns_names: Vec<String>,
    current: ArcSwap<CertificateGeneration>,
}

impl ActiveCertificateGeneration {
    #[must_use]
    pub fn new(initial: Arc<CertificateGeneration>) -> Self {
        Self {
            name: initial.metadata.name.clone(),
            dns_names: initial.metadata.dns_names.clone(),
            current: ArcSwap::from(initial),
        }
    }

    /// Returns one self-consistent generation snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Arc<CertificateGeneration> {
        self.current.load_full()
    }

    /// Publishes `replacement` only if `expected` is still the active Arc.
    ///
    /// # Errors
    ///
    /// Returns public revision metadata when another writer won the race.
    pub fn publish_if_current(
        &self,
        expected: &Arc<CertificateGeneration>,
        replacement: Arc<CertificateGeneration>,
    ) -> Result<(), CertificatePublishError> {
        if replacement.metadata.name != self.name {
            return Err(CertificatePublishError::IdentityMismatch {
                active_name: self.name.clone(),
                replacement_name: replacement.metadata.name.clone(),
            });
        }
        if replacement.metadata.dns_names != self.dns_names {
            return Err(CertificatePublishError::DnsNamesMismatch {
                identity: self.name.clone(),
                active_dns_names: self.dns_names.clone(),
                replacement_dns_names: replacement.metadata.dns_names.clone(),
            });
        }
        let previous = self.current.compare_and_swap(expected, replacement);
        if Arc::ptr_eq(&previous, expected) {
            Ok(())
        } else {
            Err(CertificatePublishError::GenerationChanged {
                expected_revision: expected.metadata.revision.clone(),
                active_revision: previous.metadata.revision.clone(),
            })
        }
    }

    pub(in super::super) fn publish_prevalidated_if_current(
        &self,
        expected: &Arc<CertificateGeneration>,
        replacement: Arc<CertificateGeneration>,
    ) -> bool {
        debug_assert_eq!(replacement.metadata.name, self.name);
        debug_assert_eq!(replacement.metadata.dns_names, self.dns_names);
        Arc::ptr_eq(
            &self.current.compare_and_swap(expected, replacement),
            expected,
        )
    }

    pub(in super::super) fn publish_transaction<T, E>(
        &self,
        gate: Option<&PublicationGate>,
        before_publish: &mut dyn FnMut(usize),
        mut load_candidate: impl FnMut() -> Result<(T, CertificateGeneration), E>,
    ) -> Result<CertificatePublication<T>, CertificatePublicationError<E>> {
        for attempt in 0..MAX_CERTIFICATE_PUBLICATION_ATTEMPTS {
            if gate.is_some_and(PublicationGate::is_stopped) {
                return Err(CertificatePublicationError::Stopped);
            }
            let (context, candidate) =
                load_candidate().map_err(CertificatePublicationError::Candidate)?;
            let expected = self.snapshot();
            let unchanged = candidate.metadata().revision == expected.metadata().revision;
            let replacement = if unchanged {
                Arc::clone(&expected)
            } else {
                Arc::new(candidate)
            };
            before_publish(attempt);
            let publish =
                || self.publish_prevalidated_if_current(&expected, Arc::clone(&replacement));
            let published = if let Some(gate) = gate {
                gate.publish(publish)
                    .ok_or(CertificatePublicationError::Stopped)?
            } else {
                publish()
            };
            if published {
                return Ok(if unchanged {
                    CertificatePublication::Unchanged(context)
                } else {
                    CertificatePublication::Activated(context)
                });
            }
        }

        Err(CertificatePublicationError::Conflict {
            attempts: MAX_CERTIFICATE_PUBLICATION_ATTEMPTS,
        })
    }
}

pub(in super::super) enum CertificatePublication<T> {
    Unchanged(T),
    Activated(T),
}

pub(in super::super) enum CertificatePublicationError<E> {
    Candidate(E),
    Conflict { attempts: usize },
    Stopped,
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

impl fmt::Debug for ActiveCertificateGeneration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActiveCertificateGeneration")
            .field("metadata", self.snapshot().metadata())
            .finish()
    }
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum CertificatePublishError {
    #[error(
        "certificate identity `{replacement_name}` cannot replace active identity `{active_name}`"
    )]
    IdentityMismatch {
        active_name: String,
        replacement_name: String,
    },
    #[error("certificate identity `{identity}` cannot change its DNS subject alternative names")]
    DnsNamesMismatch {
        identity: String,
        active_dns_names: Vec<String>,
        replacement_dns_names: Vec<String>,
    },
    #[error(
        "certificate generation changed before publication (expected {expected_revision}, active {active_revision})"
    )]
    GenerationChanged {
        expected_revision: String,
        active_revision: String,
    },
}
