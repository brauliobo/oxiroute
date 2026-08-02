use oxiroute_supervision::{
    GenerationId, Instance, InstanceId, Lifecycle, ReplacementAction, ReplacementError,
    ReplacementEvent, ReplacementSupervisor,
};

fn id(name: &str) -> InstanceId {
    InstanceId::new(name).unwrap()
}

fn instance(name: &str, generation: u64, lifecycle: Lifecycle) -> Instance {
    Instance {
        instance_id: id(name),
        generation_id: GenerationId(generation),
        lifecycle,
    }
}

fn active() -> Instance {
    instance("active-1", 1, Lifecycle::Active)
}

fn advance_to_activation(supervisor: &mut ReplacementSupervisor, candidate: &str) {
    supervisor
        .apply(ReplacementEvent::CandidateSpawned {
            instance_id: id(candidate),
        })
        .unwrap();
    supervisor
        .apply(ReplacementEvent::CandidateHandshakeComplete {
            instance_id: id(candidate),
        })
        .unwrap();
    supervisor
        .apply(ReplacementEvent::CandidatePrepared {
            instance_id: id(candidate),
        })
        .unwrap();
    supervisor
        .apply(ReplacementEvent::ActiveQuiesced {
            instance_id: supervisor.active().instance_id.clone(),
        })
        .unwrap();
}

#[test]
fn replacement_requires_drain_then_snapshot_once_before_stop() {
    let mut supervisor = ReplacementSupervisor::new(active()).unwrap();
    let candidate = instance("candidate-2", 2, Lifecycle::Spawned);

    assert_eq!(
        supervisor
            .apply(ReplacementEvent::Begin {
                candidate: candidate.clone(),
            })
            .unwrap(),
        vec![ReplacementAction::Spawn {
            instance: candidate,
        }]
    );
    advance_to_activation(&mut supervisor, "candidate-2");
    assert_eq!(
        supervisor
            .apply(ReplacementEvent::CandidateActivated {
                instance_id: id("candidate-2"),
            })
            .unwrap(),
        vec![ReplacementAction::Drain {
            instance_id: id("active-1"),
        }]
    );

    assert_eq!(supervisor.retired().unwrap().lifecycle, Lifecycle::Draining);
    assert!(matches!(
        supervisor.apply(ReplacementEvent::RetiredSnapshotCaptured {
            instance_id: id("active-1"),
        }),
        Err(ReplacementError::UnexpectedLifecycle { .. })
    ));
    assert_eq!(
        supervisor
            .apply(ReplacementEvent::RetiredDrained {
                instance_id: id("active-1"),
            })
            .unwrap(),
        vec![ReplacementAction::Snapshot {
            instance_id: id("active-1"),
        }]
    );
    assert_eq!(
        supervisor.retired().unwrap().lifecycle,
        Lifecycle::Snapshotting
    );
    assert!(matches!(
        supervisor.apply(ReplacementEvent::RetiredDrained {
            instance_id: id("active-1"),
        }),
        Err(ReplacementError::UnexpectedLifecycle { .. })
    ));
    assert_eq!(
        supervisor
            .apply(ReplacementEvent::RetiredSnapshotCaptured {
                instance_id: id("active-1"),
            })
            .unwrap(),
        vec![ReplacementAction::Terminate {
            instance_id: id("active-1"),
        }]
    );
    assert!(matches!(
        supervisor.apply(ReplacementEvent::RetiredSnapshotCaptured {
            instance_id: id("active-1"),
        }),
        Err(ReplacementError::UnexpectedLifecycle { .. })
    ));
    assert_eq!(
        supervisor
            .apply(ReplacementEvent::TerminationTimedOut {
                instance_id: id("active-1"),
            })
            .unwrap(),
        vec![ReplacementAction::Kill {
            instance_id: id("active-1"),
        }]
    );
    supervisor
        .apply(ReplacementEvent::RetiredStopped {
            instance_id: id("active-1"),
        })
        .unwrap();
    assert!(supervisor.retired().is_none());
    assert_eq!(supervisor.active().lifecycle, Lifecycle::Active);
}

#[test]
fn failed_candidate_rolls_back_without_touching_unquiesced_active() {
    let original = active();
    let mut supervisor = ReplacementSupervisor::new(original.clone()).unwrap();
    supervisor
        .apply(ReplacementEvent::Begin {
            candidate: instance("candidate-2", 2, Lifecycle::Spawned),
        })
        .unwrap();
    supervisor
        .apply(ReplacementEvent::CandidateSpawned {
            instance_id: id("candidate-2"),
        })
        .unwrap();

    assert_eq!(
        supervisor
            .apply(ReplacementEvent::CandidateFailed {
                instance_id: id("candidate-2"),
            })
            .unwrap(),
        vec![ReplacementAction::Terminate {
            instance_id: id("candidate-2"),
        }]
    );
    assert_eq!(supervisor.active(), &original);
    supervisor
        .apply(ReplacementEvent::CandidateStopped {
            instance_id: id("candidate-2"),
        })
        .unwrap();
    assert!(supervisor.candidate().is_none());
}

#[test]
fn rollback_marks_old_active_only_after_reactivation_acknowledgement() {
    let mut supervisor = ReplacementSupervisor::new(active()).unwrap();
    supervisor
        .apply(ReplacementEvent::Begin {
            candidate: instance("candidate-2", 2, Lifecycle::Spawned),
        })
        .unwrap();
    advance_to_activation(&mut supervisor, "candidate-2");

    assert_eq!(
        supervisor
            .apply(ReplacementEvent::CandidateFailed {
                instance_id: id("candidate-2"),
            })
            .unwrap(),
        vec![
            ReplacementAction::Activate {
                instance_id: id("active-1"),
            },
            ReplacementAction::Terminate {
                instance_id: id("candidate-2"),
            },
        ]
    );
    assert_eq!(supervisor.active().lifecycle, Lifecycle::Reactivating);
    assert_eq!(
        supervisor.apply(ReplacementEvent::Begin {
            candidate: instance("candidate-3", 3, Lifecycle::Spawned),
        }),
        Err(ReplacementError::ReplacementInProgress)
    );
    supervisor
        .apply(ReplacementEvent::ActiveReactivated {
            instance_id: id("active-1"),
        })
        .unwrap();
    assert_eq!(supervisor.active().lifecycle, Lifecycle::Active);
    assert!(matches!(
        supervisor.apply(ReplacementEvent::ActiveReactivated {
            instance_id: id("active-1"),
        }),
        Err(ReplacementError::Transition(_))
    ));
}

#[test]
fn stale_acknowledgements_cannot_mutate_new_candidate_or_retired_roles() {
    let mut supervisor = ReplacementSupervisor::new(active()).unwrap();
    supervisor
        .apply(ReplacementEvent::Begin {
            candidate: instance("candidate-2", 2, Lifecycle::Spawned),
        })
        .unwrap();
    supervisor
        .apply(ReplacementEvent::CandidateFailed {
            instance_id: id("candidate-2"),
        })
        .unwrap();
    supervisor
        .apply(ReplacementEvent::CandidateStopped {
            instance_id: id("candidate-2"),
        })
        .unwrap();
    supervisor
        .apply(ReplacementEvent::Begin {
            candidate: instance("candidate-3", 3, Lifecycle::Spawned),
        })
        .unwrap();

    let before = supervisor.clone();
    assert!(matches!(
        supervisor.apply(ReplacementEvent::CandidateSpawned {
            instance_id: id("candidate-2"),
        }),
        Err(ReplacementError::UnexpectedInstanceId {
            role: "candidate",
            ..
        })
    ));
    assert_eq!(supervisor, before);

    advance_to_activation(&mut supervisor, "candidate-3");
    supervisor
        .apply(ReplacementEvent::CandidateActivated {
            instance_id: id("candidate-3"),
        })
        .unwrap();
    supervisor
        .apply(ReplacementEvent::RetiredDrained {
            instance_id: id("active-1"),
        })
        .unwrap();
    supervisor
        .apply(ReplacementEvent::RetiredSnapshotCaptured {
            instance_id: id("active-1"),
        })
        .unwrap();
    supervisor
        .apply(ReplacementEvent::RetiredStopped {
            instance_id: id("active-1"),
        })
        .unwrap();
    supervisor
        .apply(ReplacementEvent::Begin {
            candidate: instance("candidate-4", 4, Lifecycle::Spawned),
        })
        .unwrap();
    advance_to_activation(&mut supervisor, "candidate-4");
    supervisor
        .apply(ReplacementEvent::CandidateActivated {
            instance_id: id("candidate-4"),
        })
        .unwrap();
    let before = supervisor.clone();
    assert!(matches!(
        supervisor.apply(ReplacementEvent::RetiredDrained {
            instance_id: id("active-1"),
        }),
        Err(ReplacementError::UnexpectedInstanceId {
            role: "retired",
            ..
        })
    ));
    assert_eq!(supervisor, before);
}

#[test]
fn supervisor_enforces_generation_identity_and_single_replacement() {
    let mut supervisor = ReplacementSupervisor::new(active()).unwrap();
    assert_eq!(
        supervisor.apply(ReplacementEvent::Begin {
            candidate: instance("old", 1, Lifecycle::Spawned),
        }),
        Err(ReplacementError::StaleGeneration {
            active: GenerationId(1),
            candidate: GenerationId(1),
        })
    );
    assert_eq!(
        supervisor.apply(ReplacementEvent::Begin {
            candidate: instance("active-1", 2, Lifecycle::Spawned),
        }),
        Err(ReplacementError::DuplicateInstanceId)
    );
    supervisor
        .apply(ReplacementEvent::Begin {
            candidate: instance("candidate-2", 2, Lifecycle::Spawned),
        })
        .unwrap();
    assert_eq!(
        supervisor.apply(ReplacementEvent::Begin {
            candidate: instance("candidate-3", 3, Lifecycle::Spawned),
        }),
        Err(ReplacementError::ReplacementInProgress)
    );
}

#[test]
fn invalid_events_do_not_mutate_supervisor_state() {
    let mut supervisor = ReplacementSupervisor::new(active()).unwrap();
    supervisor
        .apply(ReplacementEvent::Begin {
            candidate: instance("candidate-2", 2, Lifecycle::Spawned),
        })
        .unwrap();
    let before = supervisor.clone();

    assert!(matches!(
        supervisor.apply(ReplacementEvent::CandidatePrepared {
            instance_id: id("candidate-2"),
        }),
        Err(ReplacementError::UnexpectedLifecycle { .. })
    ));
    assert_eq!(supervisor, before);
}
