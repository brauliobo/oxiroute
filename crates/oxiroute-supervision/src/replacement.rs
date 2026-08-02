use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{GenerationId, InstanceId, Lifecycle, TransitionError};

/// Identity and lifecycle of one managed service instance.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Instance {
    pub instance_id: InstanceId,
    pub generation_id: GenerationId,
    pub lifecycle: Lifecycle,
}

impl Instance {
    fn transition(&mut self, next: Lifecycle) -> Result<(), TransitionError> {
        self.lifecycle = self.lifecycle.transition(next)?;
        Ok(())
    }
}

/// An external observation applied to the replacement state machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplacementEvent {
    Begin { candidate: Instance },
    CandidateSpawned { instance_id: InstanceId },
    CandidateHandshakeComplete { instance_id: InstanceId },
    CandidatePrepared { instance_id: InstanceId },
    CandidateActivated { instance_id: InstanceId },
    CandidateFailed { instance_id: InstanceId },
    CandidateStopped { instance_id: InstanceId },
    ActiveQuiesced { instance_id: InstanceId },
    ActiveReactivated { instance_id: InstanceId },
    RetiredDrained { instance_id: InstanceId },
    RetiredSnapshotCaptured { instance_id: InstanceId },
    RetiredStopped { instance_id: InstanceId },
    TerminationTimedOut { instance_id: InstanceId },
}

/// A side effect requested by the pure replacement state machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReplacementAction {
    Spawn { instance: Instance },
    Prepare { instance_id: InstanceId },
    Activate { instance_id: InstanceId },
    Quiesce { instance_id: InstanceId },
    Drain { instance_id: InstanceId },
    Snapshot { instance_id: InstanceId },
    Terminate { instance_id: InstanceId },
    Kill { instance_id: InstanceId },
}

/// Pure state for replacing one active service generation at a time.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplacementSupervisor {
    active: Instance,
    candidate: Option<Instance>,
    retired: Option<Instance>,
}

impl ReplacementSupervisor {
    /// Creates a supervisor around an already-active instance.
    ///
    /// # Errors
    ///
    /// Returns [`ReplacementError::UnexpectedLifecycle`] unless the instance is active.
    pub fn new(active: Instance) -> Result<Self, ReplacementError> {
        if active.lifecycle != Lifecycle::Active {
            return Err(ReplacementError::UnexpectedLifecycle {
                role: "active",
                expected: Lifecycle::Active,
                actual: active.lifecycle,
            });
        }
        Ok(Self {
            active,
            candidate: None,
            retired: None,
        })
    }

    /// Returns the serving instance.
    #[must_use]
    pub const fn active(&self) -> &Instance {
        &self.active
    }

    /// Returns the generation currently being prepared.
    #[must_use]
    pub const fn candidate(&self) -> Option<&Instance> {
        self.candidate.as_ref()
    }

    /// Returns the old generation being retired after activation.
    #[must_use]
    pub const fn retired(&self) -> Option<&Instance> {
        self.retired.as_ref()
    }

    /// Applies one event and returns ordered side effects for the caller to execute.
    ///
    /// State is changed only after all preconditions for an event have been checked.
    ///
    /// # Errors
    ///
    /// Returns [`ReplacementError`] when the event violates lifecycle, identity, generation, or
    /// single-replacement invariants.
    pub fn apply(
        &mut self,
        event: ReplacementEvent,
    ) -> Result<Vec<ReplacementAction>, ReplacementError> {
        match event {
            ReplacementEvent::Begin { candidate } => self.begin(candidate),
            ReplacementEvent::CandidateSpawned { instance_id } => {
                self.expect_candidate_id(&instance_id)?;
                self.transition_candidate(Lifecycle::Spawned, Lifecycle::Handshaking)?;
                Ok(Vec::new())
            }
            ReplacementEvent::CandidateHandshakeComplete { instance_id } => {
                self.expect_candidate_id(&instance_id)?;
                let instance_id =
                    self.transition_candidate(Lifecycle::Handshaking, Lifecycle::Preparing)?;
                Ok(vec![ReplacementAction::Prepare { instance_id }])
            }
            ReplacementEvent::CandidatePrepared { instance_id } => {
                self.expect_candidate_id(&instance_id)?;
                let candidate = self.candidate_mut(Lifecycle::Preparing)?;
                candidate.transition(Lifecycle::Ready)?;
                self.active.transition(Lifecycle::Quiescing)?;
                Ok(vec![ReplacementAction::Quiesce {
                    instance_id: self.active.instance_id.clone(),
                }])
            }
            ReplacementEvent::CandidateActivated { instance_id } => {
                self.expect_candidate_id(&instance_id)?;
                self.activate_candidate()
            }
            ReplacementEvent::CandidateFailed { instance_id } => {
                self.expect_candidate_id(&instance_id)?;
                self.fail_candidate()
            }
            ReplacementEvent::CandidateStopped { instance_id } => {
                self.expect_candidate_id(&instance_id)?;
                let candidate = self.candidate_mut(Lifecycle::Stopping)?;
                candidate.transition(Lifecycle::Stopped)?;
                self.candidate = None;
                Ok(Vec::new())
            }
            ReplacementEvent::ActiveQuiesced { instance_id } => {
                self.expect_active_id(&instance_id)?;
                Self::expect_lifecycle(&self.active, "active", Lifecycle::Quiescing)?;
                let candidate = self.candidate_mut(Lifecycle::Ready)?;
                candidate.transition(Lifecycle::Activating)?;
                Ok(vec![ReplacementAction::Activate {
                    instance_id: candidate.instance_id.clone(),
                }])
            }
            ReplacementEvent::ActiveReactivated { instance_id } => {
                self.expect_active_id(&instance_id)?;
                self.active.transition(Lifecycle::Active)?;
                Ok(Vec::new())
            }
            ReplacementEvent::RetiredDrained { instance_id } => {
                self.expect_retired_id(&instance_id)?;
                let retired = self.retired_mut(Lifecycle::Draining)?;
                retired.transition(Lifecycle::Snapshotting)?;
                Ok(vec![ReplacementAction::Snapshot {
                    instance_id: retired.instance_id.clone(),
                }])
            }
            ReplacementEvent::RetiredSnapshotCaptured { instance_id } => {
                self.expect_retired_id(&instance_id)?;
                let instance_id =
                    self.transition_retired(Lifecycle::Snapshotting, Lifecycle::Stopping)?;
                Ok(vec![ReplacementAction::Terminate { instance_id }])
            }
            ReplacementEvent::RetiredStopped { instance_id } => {
                self.expect_retired_id(&instance_id)?;
                let retired = self.retired_mut(Lifecycle::Stopping)?;
                retired.transition(Lifecycle::Stopped)?;
                self.retired = None;
                Ok(Vec::new())
            }
            ReplacementEvent::TerminationTimedOut { instance_id } => {
                self.ensure_stopping(&instance_id)?;
                Ok(vec![ReplacementAction::Kill { instance_id }])
            }
        }
    }

    fn begin(&mut self, candidate: Instance) -> Result<Vec<ReplacementAction>, ReplacementError> {
        if self.candidate.is_some() || self.retired.is_some() {
            return Err(ReplacementError::ReplacementInProgress);
        }
        if self.active.lifecycle != Lifecycle::Active {
            return Err(ReplacementError::ReplacementInProgress);
        }
        if candidate.lifecycle != Lifecycle::Spawned {
            return Err(ReplacementError::UnexpectedLifecycle {
                role: "candidate",
                expected: Lifecycle::Spawned,
                actual: candidate.lifecycle,
            });
        }
        if candidate.instance_id == self.active.instance_id {
            return Err(ReplacementError::DuplicateInstanceId);
        }
        if candidate.generation_id <= self.active.generation_id {
            return Err(ReplacementError::StaleGeneration {
                active: self.active.generation_id,
                candidate: candidate.generation_id,
            });
        }
        let action = ReplacementAction::Spawn {
            instance: candidate.clone(),
        };
        self.candidate = Some(candidate);
        Ok(vec![action])
    }

    fn activate_candidate(&mut self) -> Result<Vec<ReplacementAction>, ReplacementError> {
        let mut candidate = self
            .candidate
            .take()
            .ok_or(ReplacementError::MissingRole { role: "candidate" })?;
        if candidate.lifecycle != Lifecycle::Activating {
            let actual = candidate.lifecycle;
            self.candidate = Some(candidate);
            return Err(ReplacementError::UnexpectedLifecycle {
                role: "candidate",
                expected: Lifecycle::Activating,
                actual,
            });
        }
        candidate.transition(Lifecycle::Active)?;
        let mut retired = std::mem::replace(&mut self.active, candidate);
        retired.transition(Lifecycle::Draining)?;
        let instance_id = retired.instance_id.clone();
        self.retired = Some(retired);
        Ok(vec![ReplacementAction::Drain { instance_id }])
    }

    fn fail_candidate(&mut self) -> Result<Vec<ReplacementAction>, ReplacementError> {
        let candidate = self
            .candidate
            .as_mut()
            .ok_or(ReplacementError::MissingRole { role: "candidate" })?;
        candidate.transition(Lifecycle::Failed)?;
        candidate.transition(Lifecycle::Stopping)?;
        let mut actions = Vec::with_capacity(2);
        if self.active.lifecycle == Lifecycle::Quiescing {
            self.active.transition(Lifecycle::Reactivating)?;
            actions.push(ReplacementAction::Activate {
                instance_id: self.active.instance_id.clone(),
            });
        }
        actions.push(ReplacementAction::Terminate {
            instance_id: candidate.instance_id.clone(),
        });
        Ok(actions)
    }

    fn transition_candidate(
        &mut self,
        expected: Lifecycle,
        next: Lifecycle,
    ) -> Result<InstanceId, ReplacementError> {
        let candidate = self.candidate_mut(expected)?;
        candidate.transition(next)?;
        Ok(candidate.instance_id.clone())
    }

    fn transition_retired(
        &mut self,
        expected: Lifecycle,
        next: Lifecycle,
    ) -> Result<InstanceId, ReplacementError> {
        let retired = self.retired_mut(expected)?;
        retired.transition(next)?;
        Ok(retired.instance_id.clone())
    }

    fn candidate_mut(&mut self, expected: Lifecycle) -> Result<&mut Instance, ReplacementError> {
        let candidate = self
            .candidate
            .as_mut()
            .ok_or(ReplacementError::MissingRole { role: "candidate" })?;
        Self::expect_lifecycle(candidate, "candidate", expected)?;
        Ok(candidate)
    }

    fn retired_mut(&mut self, expected: Lifecycle) -> Result<&mut Instance, ReplacementError> {
        let retired = self
            .retired
            .as_mut()
            .ok_or(ReplacementError::MissingRole { role: "retired" })?;
        Self::expect_lifecycle(retired, "retired", expected)?;
        Ok(retired)
    }

    fn expect_lifecycle(
        instance: &Instance,
        role: &'static str,
        expected: Lifecycle,
    ) -> Result<(), ReplacementError> {
        if instance.lifecycle != expected {
            return Err(ReplacementError::UnexpectedLifecycle {
                role,
                expected,
                actual: instance.lifecycle,
            });
        }
        Ok(())
    }

    fn expect_candidate_id(&self, actual: &InstanceId) -> Result<(), ReplacementError> {
        let candidate = self
            .candidate
            .as_ref()
            .ok_or(ReplacementError::MissingRole { role: "candidate" })?;
        Self::expect_instance_id(candidate, "candidate", actual)
    }

    fn expect_active_id(&self, actual: &InstanceId) -> Result<(), ReplacementError> {
        Self::expect_instance_id(&self.active, "active", actual)
    }

    fn expect_retired_id(&self, actual: &InstanceId) -> Result<(), ReplacementError> {
        let retired = self
            .retired
            .as_ref()
            .ok_or(ReplacementError::MissingRole { role: "retired" })?;
        Self::expect_instance_id(retired, "retired", actual)
    }

    fn expect_instance_id(
        instance: &Instance,
        role: &'static str,
        actual: &InstanceId,
    ) -> Result<(), ReplacementError> {
        if instance.instance_id != *actual {
            return Err(ReplacementError::UnexpectedInstanceId {
                role,
                expected: instance.instance_id.clone(),
                actual: actual.clone(),
            });
        }
        Ok(())
    }

    fn ensure_stopping(&self, instance_id: &InstanceId) -> Result<(), ReplacementError> {
        let stopping = self
            .candidate
            .as_ref()
            .into_iter()
            .chain(self.retired.as_ref())
            .any(|instance| {
                instance.instance_id == *instance_id && instance.lifecycle == Lifecycle::Stopping
            });
        if stopping {
            Ok(())
        } else {
            Err(ReplacementError::NotStopping {
                instance_id: instance_id.clone(),
            })
        }
    }
}

/// A replacement event that would violate supervisor invariants.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum ReplacementError {
    #[error("a replacement is already in progress")]
    ReplacementInProgress,
    #[error("the {role} role is empty")]
    MissingRole { role: &'static str },
    #[error("{role} instance must be {expected:?}, but is {actual:?}")]
    UnexpectedLifecycle {
        role: &'static str,
        expected: Lifecycle,
        actual: Lifecycle,
    },
    #[error("candidate and active instances must have distinct IDs")]
    DuplicateInstanceId,
    #[error("{role} instance is {expected}, but acknowledgement was for {actual}")]
    UnexpectedInstanceId {
        role: &'static str,
        expected: InstanceId,
        actual: InstanceId,
    },
    #[error("candidate generation {candidate} must be newer than active generation {active}")]
    StaleGeneration {
        active: GenerationId,
        candidate: GenerationId,
    },
    #[error("instance {instance_id} is not awaiting termination")]
    NotStopping { instance_id: InstanceId },
    #[error(transparent)]
    Transition(#[from] TransitionError),
}
