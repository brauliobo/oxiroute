use oxiroute_supervision::{
    GenerationId, Instance, InstanceId, Lifecycle, ReplacementAction, ReplacementError,
    ReplacementEvent, ReplacementPhase, ReplacementSupervisor,
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

fn assert_roles(
    supervisor: &ReplacementSupervisor,
    active: Instance,
    candidate: Option<Instance>,
    retired: Option<Instance>,
) {
    let actual = (
        supervisor.active().clone(),
        supervisor.candidate().cloned(),
        supervisor.retired().cloned(),
    );
    assert_eq!(actual, (active, candidate, retired));
}

fn assert_step(
    supervisor: &mut ReplacementSupervisor,
    event: ReplacementEvent,
    actions: impl IntoIterator<Item = ReplacementAction>,
    active: Instance,
    candidate: Option<Instance>,
    retired: Option<Instance>,
) {
    assert_eq!(
        supervisor.apply(event).unwrap(),
        actions.into_iter().collect::<Vec<_>>()
    );
    assert_roles(supervisor, active, candidate, retired);
}

fn successful_replacement_at_retirement() -> ReplacementSupervisor {
    let mut supervisor = ReplacementSupervisor::new(active()).unwrap();

    assert_step(
        &mut supervisor,
        ReplacementEvent::Begin {
            candidate: instance("candidate-2", 2, Lifecycle::Spawned),
        },
        vec![ReplacementAction::Spawn {
            instance: instance("candidate-2", 2, Lifecycle::Spawned),
        }],
        active(),
        Some(instance("candidate-2", 2, Lifecycle::Spawned)),
        None,
    );
    assert_step(
        &mut supervisor,
        ReplacementEvent::CandidateSpawned {
            instance_id: id("candidate-2"),
        },
        vec![],
        active(),
        Some(instance("candidate-2", 2, Lifecycle::Handshaking)),
        None,
    );
    assert_step(
        &mut supervisor,
        ReplacementEvent::CandidateHandshakeComplete {
            instance_id: id("candidate-2"),
        },
        vec![ReplacementAction::Prepare {
            instance_id: id("candidate-2"),
        }],
        active(),
        Some(instance("candidate-2", 2, Lifecycle::Preparing)),
        None,
    );
    assert_step(
        &mut supervisor,
        ReplacementEvent::CandidatePrepared {
            instance_id: id("candidate-2"),
        },
        vec![ReplacementAction::Quiesce {
            instance_id: id("active-1"),
        }],
        instance("active-1", 1, Lifecycle::Quiescing),
        Some(instance("candidate-2", 2, Lifecycle::Ready)),
        None,
    );
    assert_step(
        &mut supervisor,
        ReplacementEvent::ActiveQuiesced {
            instance_id: id("active-1"),
        },
        vec![ReplacementAction::Activate {
            instance_id: id("candidate-2"),
        }],
        instance("active-1", 1, Lifecycle::Quiescing),
        Some(instance("candidate-2", 2, Lifecycle::Activating)),
        None,
    );
    assert_step(
        &mut supervisor,
        ReplacementEvent::CandidateActivated {
            instance_id: id("candidate-2"),
        },
        vec![ReplacementAction::Drain {
            instance_id: id("active-1"),
        }],
        instance("candidate-2", 2, Lifecycle::Active),
        None,
        Some(instance("active-1", 1, Lifecycle::Draining)),
    );
    supervisor
}

#[test]
fn success_trace_preserves_every_state_and_ordered_action() {
    let mut supervisor = successful_replacement_at_retirement();

    assert_step(
        &mut supervisor,
        ReplacementEvent::RetiredDrained {
            instance_id: id("active-1"),
        },
        vec![ReplacementAction::Snapshot {
            instance_id: id("active-1"),
        }],
        instance("candidate-2", 2, Lifecycle::Active),
        None,
        Some(instance("active-1", 1, Lifecycle::Snapshotting)),
    );
    assert_step(
        &mut supervisor,
        ReplacementEvent::RetiredSnapshotCaptured {
            instance_id: id("active-1"),
        },
        vec![ReplacementAction::Terminate {
            instance_id: id("active-1"),
        }],
        instance("candidate-2", 2, Lifecycle::Active),
        None,
        Some(instance("active-1", 1, Lifecycle::Stopping)),
    );
    assert_step(
        &mut supervisor,
        ReplacementEvent::RetiredStopped {
            instance_id: id("active-1"),
        },
        vec![],
        instance("candidate-2", 2, Lifecycle::Active),
        None,
        None,
    );
}

fn candidate_at(lifecycle: Lifecycle) -> ReplacementSupervisor {
    let mut supervisor = ReplacementSupervisor::new(active()).unwrap();
    supervisor
        .apply(ReplacementEvent::Begin {
            candidate: instance("candidate-2", 2, Lifecycle::Spawned),
        })
        .unwrap();
    if lifecycle == Lifecycle::Spawned {
        return supervisor;
    }
    supervisor
        .apply(ReplacementEvent::CandidateSpawned {
            instance_id: id("candidate-2"),
        })
        .unwrap();
    if lifecycle == Lifecycle::Handshaking {
        return supervisor;
    }
    supervisor
        .apply(ReplacementEvent::CandidateHandshakeComplete {
            instance_id: id("candidate-2"),
        })
        .unwrap();
    if lifecycle == Lifecycle::Preparing {
        return supervisor;
    }
    supervisor
        .apply(ReplacementEvent::CandidatePrepared {
            instance_id: id("candidate-2"),
        })
        .unwrap();
    if lifecycle == Lifecycle::Ready {
        return supervisor;
    }
    supervisor
        .apply(ReplacementEvent::ActiveQuiesced {
            instance_id: id("active-1"),
        })
        .unwrap();
    assert_eq!(lifecycle, Lifecycle::Activating);
    supervisor
}

#[test]
fn candidate_failure_trace_covers_every_pre_and_post_quiesce_phase() {
    for lifecycle in [
        Lifecycle::Spawned,
        Lifecycle::Handshaking,
        Lifecycle::Preparing,
        Lifecycle::Ready,
        Lifecycle::Activating,
    ] {
        let mut supervisor = candidate_at(lifecycle);
        let after_quiesce = matches!(lifecycle, Lifecycle::Ready | Lifecycle::Activating);
        let actions = if after_quiesce {
            vec![
                ReplacementAction::Activate {
                    instance_id: id("active-1"),
                },
                ReplacementAction::Terminate {
                    instance_id: id("candidate-2"),
                },
            ]
        } else {
            vec![ReplacementAction::Terminate {
                instance_id: id("candidate-2"),
            }]
        };

        assert_step(
            &mut supervisor,
            ReplacementEvent::CandidateFailed {
                instance_id: id("candidate-2"),
            },
            actions,
            instance(
                "active-1",
                1,
                if after_quiesce {
                    Lifecycle::Reactivating
                } else {
                    Lifecycle::Active
                },
            ),
            Some(instance("candidate-2", 2, Lifecycle::Stopping)),
            None,
        );
    }
}

#[test]
fn rollback_completes_in_reactivation_then_reap_order() {
    let mut supervisor = candidate_at(Lifecycle::Activating);
    supervisor
        .apply(ReplacementEvent::CandidateFailed {
            instance_id: id("candidate-2"),
        })
        .unwrap();

    assert_step(
        &mut supervisor,
        ReplacementEvent::ActiveReactivated {
            instance_id: id("active-1"),
        },
        vec![],
        active(),
        Some(instance("candidate-2", 2, Lifecycle::Stopping)),
        None,
    );
    assert_step(
        &mut supervisor,
        ReplacementEvent::CandidateStopped {
            instance_id: id("candidate-2"),
        },
        vec![],
        active(),
        None,
        None,
    );
}

#[test]
fn rollback_completes_in_reap_then_reactivation_order() {
    let mut supervisor = candidate_at(Lifecycle::Ready);
    supervisor
        .apply(ReplacementEvent::CandidateFailed {
            instance_id: id("candidate-2"),
        })
        .unwrap();

    assert_step(
        &mut supervisor,
        ReplacementEvent::CandidateStopped {
            instance_id: id("candidate-2"),
        },
        vec![],
        instance("active-1", 1, Lifecycle::Reactivating),
        None,
        None,
    );
    assert_step(
        &mut supervisor,
        ReplacementEvent::ActiveReactivated {
            instance_id: id("active-1"),
        },
        vec![],
        active(),
        None,
        None,
    );
}

fn retired_at(lifecycle: Lifecycle) -> ReplacementSupervisor {
    let mut supervisor = candidate_at(Lifecycle::Activating);
    supervisor
        .apply(ReplacementEvent::CandidateActivated {
            instance_id: id("candidate-2"),
        })
        .unwrap();
    if lifecycle == Lifecycle::Draining {
        return supervisor;
    }
    supervisor
        .apply(ReplacementEvent::RetiredDrained {
            instance_id: id("active-1"),
        })
        .unwrap();
    if lifecycle == Lifecycle::Snapshotting {
        return supervisor;
    }
    supervisor
        .apply(ReplacementEvent::RetiredSnapshotCaptured {
            instance_id: id("active-1"),
        })
        .unwrap();
    assert_eq!(lifecycle, Lifecycle::Stopping);
    supervisor
}

#[test]
fn retired_failure_transitions_every_owned_state_to_forced_stopping() {
    for lifecycle in [Lifecycle::Draining, Lifecycle::Snapshotting] {
        let mut supervisor = retired_at(lifecycle);
        let before = supervisor.clone();
        assert_eq!(
            supervisor.apply(ReplacementEvent::TerminationTimedOut {
                instance_id: id("active-1"),
            }),
            Err(ReplacementError::NotStopping {
                instance_id: id("active-1"),
            })
        );
        assert_eq!(supervisor, before);
    }

    for lifecycle in [
        Lifecycle::Draining,
        Lifecycle::Snapshotting,
        Lifecycle::Stopping,
    ] {
        let mut supervisor = retired_at(lifecycle);
        assert_step(
            &mut supervisor,
            ReplacementEvent::RetiredFailed {
                instance_id: id("active-1"),
            },
            vec![ReplacementAction::Kill {
                instance_id: id("active-1"),
            }],
            instance("candidate-2", 2, Lifecycle::Active),
            None,
            Some(instance("active-1", 1, Lifecycle::Stopping)),
        );

        for _ in 0..2 {
            assert_step(
                &mut supervisor,
                ReplacementEvent::TerminationTimedOut {
                    instance_id: id("active-1"),
                },
                vec![ReplacementAction::Kill {
                    instance_id: id("active-1"),
                }],
                instance("candidate-2", 2, Lifecycle::Active),
                None,
                Some(instance("active-1", 1, Lifecycle::Stopping)),
            );
        }
    }

    let mut supervisor = ReplacementSupervisor::new(active()).unwrap();
    let before = supervisor.clone();
    assert_eq!(
        supervisor.apply(ReplacementEvent::RetiredFailed {
            instance_id: id("active-1"),
        }),
        Err(ReplacementError::MissingRole { role: "retired" })
    );
    assert_eq!(supervisor, before);
}

#[test]
fn stale_and_wrong_role_ids_are_rejected_atomically() {
    let mut supervisor = candidate_at(Lifecycle::Ready);
    for event in [
        ReplacementEvent::CandidateFailed {
            instance_id: id("active-1"),
        },
        ReplacementEvent::ActiveQuiesced {
            instance_id: id("candidate-2"),
        },
        ReplacementEvent::CandidatePrepared {
            instance_id: id("stale"),
        },
    ] {
        let before = supervisor.clone();
        assert!(matches!(
            supervisor.apply(event),
            Err(ReplacementError::UnexpectedInstanceId { .. })
        ));
        assert_eq!(supervisor, before);
    }

    let mut supervisor = retired_at(Lifecycle::Draining);
    for event in [
        ReplacementEvent::RetiredDrained {
            instance_id: id("candidate-2"),
        },
        ReplacementEvent::RetiredFailed {
            instance_id: id("candidate-2"),
        },
    ] {
        let before = supervisor.clone();
        assert!(matches!(
            supervisor.apply(event),
            Err(ReplacementError::UnexpectedInstanceId {
                role: "retired",
                ..
            })
        ));
        assert_eq!(supervisor, before);
    }
}

#[test]
fn failed_generation_can_be_reused_after_rollback_completes() {
    let mut supervisor = candidate_at(Lifecycle::Activating);
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
        .apply(ReplacementEvent::ActiveReactivated {
            instance_id: id("active-1"),
        })
        .unwrap();

    assert_step(
        &mut supervisor,
        ReplacementEvent::Begin {
            candidate: instance("candidate-2-retry", 2, Lifecycle::Spawned),
        },
        vec![ReplacementAction::Spawn {
            instance: instance("candidate-2-retry", 2, Lifecycle::Spawned),
        }],
        active(),
        Some(instance("candidate-2-retry", 2, Lifecycle::Spawned)),
        None,
    );
}

fn bounded_event_corpus() -> Vec<ReplacementEvent> {
    let mut events = vec![
        ReplacementEvent::Begin {
            candidate: instance("candidate-2", 2, Lifecycle::Spawned),
        },
        ReplacementEvent::Begin {
            candidate: instance("candidate-3", 3, Lifecycle::Spawned),
        },
        ReplacementEvent::Begin {
            candidate: instance("stale-generation", 1, Lifecycle::Spawned),
        },
        ReplacementEvent::Begin {
            candidate: instance("active-1", 4, Lifecycle::Spawned),
        },
        ReplacementEvent::Begin {
            candidate: instance("wrong-lifecycle", 4, Lifecycle::Handshaking),
        },
    ];
    for instance_id in ["active-1", "candidate-2", "candidate-3", "stale"] {
        let instance_id = id(instance_id);
        events.extend([
            ReplacementEvent::CandidateSpawned {
                instance_id: instance_id.clone(),
            },
            ReplacementEvent::CandidateHandshakeComplete {
                instance_id: instance_id.clone(),
            },
            ReplacementEvent::CandidatePrepared {
                instance_id: instance_id.clone(),
            },
            ReplacementEvent::CandidateActivated {
                instance_id: instance_id.clone(),
            },
            ReplacementEvent::CandidateFailed {
                instance_id: instance_id.clone(),
            },
            ReplacementEvent::CandidateStopped {
                instance_id: instance_id.clone(),
            },
            ReplacementEvent::ActiveQuiesced {
                instance_id: instance_id.clone(),
            },
            ReplacementEvent::ActiveReactivated {
                instance_id: instance_id.clone(),
            },
            ReplacementEvent::RetiredDrained {
                instance_id: instance_id.clone(),
            },
            ReplacementEvent::RetiredSnapshotCaptured {
                instance_id: instance_id.clone(),
            },
            ReplacementEvent::RetiredFailed {
                instance_id: instance_id.clone(),
            },
            ReplacementEvent::RetiredStopped {
                instance_id: instance_id.clone(),
            },
            ReplacementEvent::TerminationTimedOut { instance_id },
        ]);
    }
    events
}

fn characterized_phase(supervisor: &ReplacementSupervisor) -> ReplacementPhase {
    if let Some(retired) = supervisor.retired() {
        return match retired.lifecycle {
            Lifecycle::Draining | Lifecycle::Snapshotting => ReplacementPhase::DrainingRetired,
            Lifecycle::Stopping => ReplacementPhase::StoppingRetired,
            lifecycle => panic!("uncharacterized retired lifecycle: {lifecycle:?}"),
        };
    }
    if let Some(candidate) = supervisor.candidate() {
        return match candidate.lifecycle {
            Lifecycle::Spawned | Lifecycle::Handshaking | Lifecycle::Preparing => {
                ReplacementPhase::AdoptingCandidate
            }
            Lifecycle::Ready => ReplacementPhase::Quiescing,
            Lifecycle::Activating => ReplacementPhase::ActivatingCandidate,
            Lifecycle::Stopping => ReplacementPhase::RollingBack,
            lifecycle => panic!("uncharacterized candidate lifecycle: {lifecycle:?}"),
        };
    }
    match supervisor.active().lifecycle {
        Lifecycle::Active => ReplacementPhase::Running,
        Lifecycle::Reactivating => ReplacementPhase::RollingBack,
        lifecycle => panic!("uncharacterized active lifecycle: {lifecycle:?}"),
    }
}

#[test]
fn every_invalid_event_is_atomic_over_bounded_reachable_states() {
    const MAX_REACHABLE_STATES: usize = 64;

    let mut reachable = vec![ReplacementSupervisor::new(active()).unwrap()];
    let mut cursor = 0;
    while cursor < reachable.len() {
        let state = reachable[cursor].clone();
        assert_eq!(state.phase(), characterized_phase(&state));
        for event in bounded_event_corpus() {
            let mut next = state.clone();
            if next.apply(event).is_err() {
                assert_eq!(next, state);
            } else if !reachable.contains(&next) {
                assert!(
                    reachable.len() < MAX_REACHABLE_STATES,
                    "bounded model exceeded {MAX_REACHABLE_STATES} states"
                );
                reachable.push(next);
            }
        }
        cursor += 1;
    }

    assert_eq!(reachable.len(), 57);
    for phase in [
        ReplacementPhase::Running,
        ReplacementPhase::AdoptingCandidate,
        ReplacementPhase::Quiescing,
        ReplacementPhase::ActivatingCandidate,
        ReplacementPhase::RollingBack,
        ReplacementPhase::DrainingRetired,
        ReplacementPhase::StoppingRetired,
    ] {
        assert!(reachable.iter().any(|state| state.phase() == phase));
    }
}
