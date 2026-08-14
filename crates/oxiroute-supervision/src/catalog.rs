use thiserror::Error;

use crate::{GenerationId, InstanceId};

/// The role occupied by a supervised generation launch document.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GenerationRole {
    /// The generation currently serving traffic.
    Active,
    /// The generation being prepared for publication.
    Candidate,
    /// The generation retained after the most recent publication.
    Previous,
    /// A candidate that failed and must not be retried implicitly.
    Quarantined,
    /// A configuration that cannot be activated without a process restart.
    RestartRequired,
}

/// Immutable inputs needed to launch one supervised generation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GenerationLaunchDocument<R, T> {
    instance_id: InstanceId,
    generation_id: GenerationId,
    revision: R,
    payload: T,
}

impl<R, T> GenerationLaunchDocument<R, T> {
    /// Creates a launch document without acquiring or starting any runtime resource.
    #[must_use]
    pub const fn new(
        instance_id: InstanceId,
        generation_id: GenerationId,
        revision: R,
        payload: T,
    ) -> Self {
        Self {
            instance_id,
            generation_id,
            revision,
            payload,
        }
    }

    /// Returns the process instance identity assigned to the document.
    #[must_use]
    pub const fn instance_id(&self) -> &InstanceId {
        &self.instance_id
    }

    /// Returns the logical generation identity assigned to the document.
    #[must_use]
    pub const fn generation_id(&self) -> GenerationId {
        self.generation_id
    }

    /// Returns the effective revision represented by the document.
    #[must_use]
    pub const fn revision(&self) -> &R {
        &self.revision
    }

    /// Returns the launch payload without transferring ownership.
    #[must_use]
    pub const fn payload(&self) -> &T {
        &self.payload
    }

    /// Consumes the document and returns its launch payload.
    #[must_use]
    pub fn into_payload(self) -> T {
        self.payload
    }
}

/// Pure role and launch-document state for one supervised service.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SupervisedGenerationCatalog<R, T> {
    active: GenerationLaunchDocument<R, T>,
    candidate: Option<GenerationLaunchDocument<R, T>>,
    previous: Option<GenerationLaunchDocument<R, T>>,
    quarantined: Option<GenerationLaunchDocument<R, T>>,
    restart_required: Option<GenerationLaunchDocument<R, T>>,
    next_generation: Option<GenerationId>,
}

impl<R, T> SupervisedGenerationCatalog<R, T> {
    /// Creates a catalog with one already-active launch document.
    #[must_use]
    pub fn new(active: GenerationLaunchDocument<R, T>) -> Self {
        Self {
            next_generation: active.generation_id.0.checked_add(1).map(GenerationId),
            active,
            candidate: None,
            previous: None,
            quarantined: None,
            restart_required: None,
        }
    }

    /// Returns the active launch document.
    #[must_use]
    pub const fn active(&self) -> &GenerationLaunchDocument<R, T> {
        &self.active
    }

    /// Returns the candidate launch document, if one is pending.
    #[must_use]
    pub const fn candidate(&self) -> Option<&GenerationLaunchDocument<R, T>> {
        self.candidate.as_ref()
    }

    /// Returns the previous launch document, if one is retained.
    #[must_use]
    pub const fn previous(&self) -> Option<&GenerationLaunchDocument<R, T>> {
        self.previous.as_ref()
    }

    /// Returns the quarantined launch document, if one is retained.
    #[must_use]
    pub const fn quarantined(&self) -> Option<&GenerationLaunchDocument<R, T>> {
        self.quarantined.as_ref()
    }

    /// Returns the launch document that requires a process restart, if one is retained.
    #[must_use]
    pub const fn restart_required(&self) -> Option<&GenerationLaunchDocument<R, T>> {
        self.restart_required.as_ref()
    }

    /// Returns the document occupying `role`.
    #[must_use]
    pub const fn get(&self, role: GenerationRole) -> Option<&GenerationLaunchDocument<R, T>> {
        match role {
            GenerationRole::Active => Some(&self.active),
            GenerationRole::Candidate => self.candidate.as_ref(),
            GenerationRole::Previous => self.previous.as_ref(),
            GenerationRole::Quarantined => self.quarantined.as_ref(),
            GenerationRole::RestartRequired => self.restart_required.as_ref(),
        }
    }

    /// Reserves the next monotonic generation identity.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::GenerationExhausted`] after the numeric identity space is used up.
    pub fn allocate_generation(&mut self) -> Result<GenerationId, CatalogError> {
        let generation = self
            .next_generation
            .ok_or(CatalogError::GenerationExhausted)?;
        self.next_generation = generation.0.checked_add(1).map(GenerationId);
        Ok(generation)
    }

    /// Adds a newer launch document as the sole pending candidate.
    ///
    /// # Errors
    ///
    /// Returns an error when another candidate exists, the generation is not newer than every
    /// retained document, or the instance identity is already in use.
    pub fn begin_candidate(
        &mut self,
        candidate: GenerationLaunchDocument<R, T>,
    ) -> Result<(), CatalogError> {
        if self.candidate.is_some() {
            return Err(CatalogError::CandidateInProgress);
        }
        if self.contains_instance_id(candidate.instance_id()) {
            return Err(CatalogError::DuplicateInstanceId {
                instance_id: candidate.instance_id.clone(),
            });
        }
        let current = self.latest_generation();
        if candidate.generation_id <= current {
            return Err(CatalogError::StaleGeneration {
                current,
                candidate: candidate.generation_id,
            });
        }
        self.advance_generation_allocator(candidate.generation_id);
        self.candidate = Some(candidate);
        Ok(())
    }

    /// Publishes the candidate and retains the former active document as previous.
    ///
    /// The displaced previous document is returned so its owner can release it explicitly.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::MissingRole`] when no candidate is pending.
    pub fn commit_candidate(
        &mut self,
    ) -> Result<Option<GenerationLaunchDocument<R, T>>, CatalogError> {
        let candidate = self.candidate.take().ok_or(CatalogError::MissingRole {
            role: GenerationRole::Candidate,
        })?;
        let active = std::mem::replace(&mut self.active, candidate);
        Ok(self.previous.replace(active))
    }

    /// Moves the pending candidate into quarantine without changing the active generation.
    ///
    /// The displaced quarantined document is returned for explicit release.
    ///
    /// # Errors
    ///
    /// Returns [`CatalogError::MissingRole`] when no candidate is pending.
    pub fn quarantine_candidate(
        &mut self,
    ) -> Result<Option<GenerationLaunchDocument<R, T>>, CatalogError> {
        let candidate = self.candidate.take().ok_or(CatalogError::MissingRole {
            role: GenerationRole::Candidate,
        })?;
        Ok(self.quarantined.replace(candidate))
    }

    /// Records a launch document whose topology requires a process restart.
    ///
    /// The displaced restart-required document is returned for explicit release.
    ///
    /// # Errors
    ///
    /// Returns an error when the document reuses an identity or is not newer than retained state.
    pub fn record_restart_required(
        &mut self,
        document: GenerationLaunchDocument<R, T>,
    ) -> Result<Option<GenerationLaunchDocument<R, T>>, CatalogError> {
        if self.contains_instance_id(document.instance_id()) {
            return Err(CatalogError::DuplicateInstanceId {
                instance_id: document.instance_id.clone(),
            });
        }
        let current = self.latest_generation();
        if document.generation_id <= current {
            return Err(CatalogError::StaleGeneration {
                current,
                candidate: document.generation_id,
            });
        }
        self.advance_generation_allocator(document.generation_id);
        Ok(self.restart_required.replace(document))
    }

    /// Removes and returns the retained previous launch document.
    #[must_use]
    pub fn take_previous(&mut self) -> Option<GenerationLaunchDocument<R, T>> {
        self.previous.take()
    }

    /// Removes and returns the retained quarantined launch document.
    #[must_use]
    pub fn take_quarantined(&mut self) -> Option<GenerationLaunchDocument<R, T>> {
        self.quarantined.take()
    }

    /// Removes and returns the retained restart-required launch document.
    #[must_use]
    pub fn take_restart_required(&mut self) -> Option<GenerationLaunchDocument<R, T>> {
        self.restart_required.take()
    }

    fn contains_instance_id(&self, instance_id: &InstanceId) -> bool {
        self.active.instance_id().eq(instance_id)
            || self
                .candidate
                .iter()
                .chain(self.previous.iter())
                .chain(self.quarantined.iter())
                .chain(self.restart_required.iter())
                .any(|document| document.instance_id() == instance_id)
    }

    fn latest_generation(&self) -> GenerationId {
        let retained = self
            .candidate
            .iter()
            .chain(self.previous.iter())
            .chain(self.quarantined.iter())
            .chain(self.restart_required.iter())
            .map(GenerationLaunchDocument::generation_id)
            .fold(self.active.generation_id(), GenerationId::max);
        let allocated = self
            .next_generation
            .map_or(GenerationId(u64::MAX), |next| GenerationId(next.0 - 1));
        retained.max(allocated)
    }

    fn advance_generation_allocator(&mut self, generation: GenerationId) {
        if self.next_generation.is_some_and(|next| generation >= next) {
            self.next_generation = generation.0.checked_add(1).map(GenerationId);
        }
    }
}

/// A catalog invariant violation.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum CatalogError {
    /// A candidate already occupies the pending slot.
    #[error("a supervised generation candidate is already in progress")]
    CandidateInProgress,
    /// No numeric generation identity remains.
    #[error("supervised generation identities are exhausted")]
    GenerationExhausted,
    /// A role-dependent operation was requested without that role.
    #[error("the {role:?} generation role is empty")]
    MissingRole { role: GenerationRole },
    /// A launch document reused an instance identity.
    #[error("generation instance {instance_id} is already catalogued")]
    DuplicateInstanceId { instance_id: InstanceId },
    /// A launch document did not advance the retained generation sequence.
    #[error("generation {candidate} must be newer than retained generation {current}")]
    StaleGeneration {
        current: GenerationId,
        candidate: GenerationId,
    },
}
