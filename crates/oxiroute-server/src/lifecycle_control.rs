use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

use oxiroute_supervision::{LifecycleControl, LifecycleOperation, LifecycleRequest};

use crate::{
    AdministrativeState, GenerationError, GenerationManager, GenerationStatus,
    RuntimeReferenceKind,
    config_coordinator::{
        CanonicalConfigCoordinator, ConfigLoadOutcome, ConfigLoadRejection, EffectiveRevision,
    },
    generation::GENERATION_PREPARATION_TIMEOUT,
};

pub(crate) type LifecyclePort = dyn LifecycleControl<
        Revision = String,
        Status = GenerationStatus,
        Outcome = LifecycleOutcome,
        Error = LifecycleError,
    > + Send
    + Sync;

#[derive(Debug)]
pub(crate) enum LifecycleOutcome {
    Prepared(EffectiveRevision),
    Drained {
        drained: bool,
        active_references: u64,
    },
    ShutdownRequested,
}

#[derive(Debug)]
pub(crate) enum LifecycleError {
    Mutation(GenerationError),
    ConfigRejected {
        rejection: ConfigLoadRejection,
        active_revision: Option<EffectiveRevision>,
    },
    Preparation(GenerationError),
    Rollback(GenerationError),
    InvalidDrainTimeout,
    ShutdownUnavailable,
}

pub(crate) struct DirectLifecycleControl {
    coordinator: CanonicalConfigCoordinator,
    generations: GenerationManager,
    process_shutdown: Option<Arc<AtomicBool>>,
}

impl DirectLifecycleControl {
    pub(crate) fn new(
        coordinator: CanonicalConfigCoordinator,
        generations: GenerationManager,
        process_shutdown: Option<Arc<AtomicBool>>,
    ) -> Self {
        Self {
            coordinator,
            generations,
            process_shutdown,
        }
    }

    fn begin_mutation(
        &self,
        expected_revision: &str,
    ) -> Result<crate::GenerationMutation, LifecycleError> {
        let expected_revision = expected_revision
            .parse()
            .map_err(|_| LifecycleError::Mutation(GenerationError::RevisionConflict))?;
        self.generations
            .begin_mutation(&expected_revision)
            .map_err(LifecycleError::Mutation)
    }

    fn reload(&self, expected_revision: &str) -> Result<LifecycleOutcome, LifecycleError> {
        let _mutation = self.begin_mutation(expected_revision)?;
        let document = match self.coordinator.load() {
            ConfigLoadOutcome::Loaded(document) => document,
            ConfigLoadOutcome::Rejected(rejection) => {
                return Err(LifecycleError::ConfigRejected {
                    rejection,
                    active_revision: self.generations.status().active_revision,
                });
            }
        };
        self.generations
            .prepare_with_deadline(*document, Instant::now() + GENERATION_PREPARATION_TIMEOUT)
            .map(|candidate| LifecycleOutcome::Prepared(candidate.revision().candidate.clone()))
            .map_err(LifecycleError::Preparation)
    }

    fn rollback(&self, expected_revision: &str) -> Result<LifecycleOutcome, LifecycleError> {
        let _mutation = self.begin_mutation(expected_revision)?;
        self.generations
            .rollback_with_deadline(Instant::now() + GENERATION_PREPARATION_TIMEOUT)
            .map(|candidate| LifecycleOutcome::Prepared(candidate.revision().candidate.clone()))
            .map_err(LifecycleError::Rollback)
    }

    fn drain(
        &self,
        expected_revision: &str,
        timeout: Option<Duration>,
    ) -> Result<LifecycleOutcome, LifecycleError> {
        let mutation = self.begin_mutation(expected_revision)?;
        if timeout.is_some_and(|timeout| timeout > Duration::from_mins(5)) {
            return Err(LifecycleError::InvalidDrainTimeout);
        }
        let active = mutation.generation();
        active.stop_accepting();
        Ok(LifecycleOutcome::Drained {
            drained: active.drained(),
            active_references: active_reference_count(active),
        })
    }

    fn shutdown(&self, expected_revision: &str) -> Result<LifecycleOutcome, LifecycleError> {
        let Some(shutdown) = &self.process_shutdown else {
            return Err(LifecycleError::ShutdownUnavailable);
        };
        let mutation = self.begin_mutation(expected_revision)?;
        drop(
            self.generations
                .begin_shutdown(Instant::now() + Duration::from_secs(5)),
        );
        mutation
            .generation()
            .metrics()
            .set_process_administrative_state(AdministrativeState::Drain);
        shutdown.store(true, Ordering::Release);
        crate::operational_event::emit("process_shutdown", "requested", None);
        Ok(LifecycleOutcome::ShutdownRequested)
    }
}

impl LifecycleControl for DirectLifecycleControl {
    type Revision = String;
    type Status = GenerationStatus;
    type Outcome = LifecycleOutcome;
    type Error = LifecycleError;

    fn status(&self) -> Self::Status {
        self.generations.status()
    }

    fn execute(
        &self,
        request: LifecycleRequest<Self::Revision>,
        timeout: Option<Duration>,
    ) -> Result<Self::Outcome, Self::Error> {
        match request.operation() {
            LifecycleOperation::Reload => self.reload(request.expected_revision()),
            LifecycleOperation::Rollback => self.rollback(request.expected_revision()),
            LifecycleOperation::Drain => self.drain(request.expected_revision(), timeout),
            LifecycleOperation::Shutdown => self.shutdown(request.expected_revision()),
        }
    }
}

fn active_reference_count(active: &crate::RuntimeGeneration) -> u64 {
    use RuntimeReferenceKind::{
        ForwardHttp1, ForwardHttp3, Http1, Http2, Http3, Rtmp, Tcp, Udp, WebSocket,
    };
    [
        ForwardHttp1,
        ForwardHttp3,
        Http1,
        Http2,
        Http3,
        WebSocket,
        Tcp,
        Rtmp,
        Udp,
    ]
    .into_iter()
    .map(|kind| active.active_references(kind))
    .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn control(process_shutdown: Option<Arc<AtomicBool>>) -> DirectLifecycleControl {
        DirectLifecycleControl::new(
            CanonicalConfigCoordinator::new("lifecycle-contract.lua").expect("coordinator"),
            GenerationManager::new(),
            process_shutdown,
        )
    }

    #[test]
    fn direct_control_projects_manager_status_and_revision_conflicts_for_mutations() {
        let control = control(Some(Arc::new(AtomicBool::new(false))));
        let status = control.status();

        assert_eq!(status.active_revision, None);
        for request in [
            control.request_reload(&"invalid".into()),
            control.request_rollback(&"invalid".into()),
            control.request_drain(&"invalid".into()),
            control.request_shutdown(&"invalid".into()),
        ] {
            assert!(matches!(
                control.execute(request, None),
                Err(LifecycleError::Mutation(GenerationError::RevisionConflict))
            ));
        }
    }

    #[test]
    fn direct_control_preserves_shutdown_availability_precedence() {
        let control = control(None);

        assert!(matches!(
            control.execute(control.request_shutdown(&"invalid".into()), None),
            Err(LifecycleError::ShutdownUnavailable)
        ));
    }
}
