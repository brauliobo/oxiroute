#![allow(dead_code, unused_imports, clippy::duplicate_mod)]

#[path = "support/config.rs"]
mod config_support;

use std::{sync::Arc, thread, time::Duration};

use oxiroute_config::{ConfigDraft, Retirement};
use oxiroute_server::{
    GenerationCandidate, GenerationManager, GenerationReference, RuntimeGeneration,
    RuntimeReferenceKind,
    config_coordinator::{AuthoredRevision, EffectiveRevision, ResolvedConfigDocument},
};

fn document(sequence: u64, config: &ConfigDraft) -> ResolvedConfigDocument {
    let revision = format!("{sequence:064x}");
    ResolvedConfigDocument {
        authored_revision: revision.parse::<AuthoredRevision>().unwrap(),
        effective_revision: revision.parse::<EffectiveRevision>().unwrap(),
        validated_config: config.clone().validate().expect("valid config"),
        format: oxiroute_config_source::ConfigFormat::Lua,
        compositional: false,
        dependencies: Vec::new(),
        config_preview: String::new(),
        diagnostics: Vec::new(),
    }
}

fn config(retirement: Option<Retirement>) -> ConfigDraft {
    ConfigDraft {
        retirement,
        ..config_support::empty_config()
    }
}

fn activate(
    manager: &GenerationManager,
    sequence: u64,
    config: &ConfigDraft,
) -> Arc<RuntimeGeneration> {
    let candidate = manager
        .prepare(document(sequence, config))
        .expect("prepare");
    let mut startup = manager.begin_candidate_start(&candidate).expect("start");
    let generation = startup.claim_runtime_start().expect("claim");
    assert!(generation.mark_runtime_started());
    startup.activate().expect("activate")
}

fn candidate(
    manager: &GenerationManager,
    sequence: u64,
    config: &ConfigDraft,
) -> GenerationCandidate {
    manager
        .prepare(document(sequence, config))
        .expect("prepare")
}

fn hold(generation: &Arc<RuntimeGeneration>, kind: RuntimeReferenceKind) -> GenerationReference {
    generation.begin_owned_reference(kind)
}

fn limited(deadline_ms: u64, cap: u64) -> ConfigDraft {
    config(Some(Retirement {
        drain_deadline_ms: Some(deadline_ms),
        max_retired_generations: cap,
    }))
}

#[test]
fn a_replaced_generation_with_work_in_flight_is_listed_until_it_drains() {
    let manager = GenerationManager::new();
    let first = activate(&manager, 1, &config(None));
    let request = hold(&first, RuntimeReferenceKind::Http1);
    let relay = hold(&first, RuntimeReferenceKind::Tcp);
    activate(&manager, 2, &config(None));

    let status = manager.status();
    assert_eq!(status.retired_generations.len(), 1);
    let retired = &status.retired_generations[0];
    assert_eq!(retired.revision, first.revision().candidate);
    assert_eq!(retired.state.as_str(), "draining");
    assert_eq!(retired.in_flight, 2);
    assert_eq!(retired.in_flight_by_kind.get("http1"), Some(&1));
    assert_eq!(retired.in_flight_by_kind.get("tcp"), Some(&1));
    assert!(
        retired.deadline_remaining_seconds > 10_000,
        "default is three hours"
    );
    assert_eq!(status.max_retired_generations, 4);

    drop(request);
    assert_eq!(manager.status().retired_generations[0].in_flight, 1);
    drop(relay);
    assert!(manager.status().retired_generations.is_empty());
}

#[test]
fn a_replaced_generation_without_work_is_never_listed() {
    let manager = GenerationManager::new();
    activate(&manager, 1, &config(None));
    activate(&manager, 2, &config(None));

    assert!(manager.status().retired_generations.is_empty());
}

#[test]
fn retired_generations_are_listed_oldest_first() {
    let manager = GenerationManager::new();
    let first = activate(&manager, 1, &config(None));
    let _first_request = hold(&first, RuntimeReferenceKind::Http1);
    let second = activate(&manager, 2, &config(None));
    let _second_request = hold(&second, RuntimeReferenceKind::Http2);
    activate(&manager, 3, &config(None));

    let revisions = manager
        .status()
        .retired_generations
        .into_iter()
        .map(|retired| retired.revision)
        .collect::<Vec<_>>();
    assert_eq!(
        revisions,
        [
            first.revision().candidate.clone(),
            second.revision().candidate.clone()
        ]
    );
}

#[test]
fn the_deadline_follows_the_retiring_generations_own_config() {
    let manager = GenerationManager::new();
    let first = activate(&manager, 1, &limited(60_000, 4));
    let _request = hold(&first, RuntimeReferenceKind::Http1);
    activate(&manager, 2, &limited(86_400_000, 4));

    let remaining = manager.status().retired_generations[0].deadline_remaining_seconds;
    assert!((55..=60).contains(&remaining), "{remaining}");
}

#[test]
fn a_generation_past_its_deadline_is_closed_once_and_counted() {
    let manager = GenerationManager::new();
    let first = activate(&manager, 1, &limited(1_000, 4));
    let request = hold(&first, RuntimeReferenceKind::WebSocket);
    activate(&manager, 2, &config(None));

    assert!(manager.expire_retired().is_empty(), "before the deadline");
    thread::sleep(Duration::from_millis(1_100));
    let expired = manager.expire_retired();
    assert_eq!(expired.len(), 1);
    assert!(Arc::ptr_eq(&expired[0], &first));
    assert!(
        manager.expire_retired().is_empty(),
        "each generation is closed once"
    );
    assert_eq!(
        manager.status().retired_generations[0].state.as_str(),
        "closing"
    );
    assert_eq!(manager.retirement_counters().forced_close_drain_deadline, 1);

    drop(request);
    assert!(manager.status().retired_generations.is_empty());
}

#[test]
fn a_full_registry_defers_the_next_apply_until_a_slot_frees() {
    let manager = GenerationManager::new();
    let first = activate(&manager, 1, &limited(3_600_000, 1));
    let request = hold(&first, RuntimeReferenceKind::Http1);
    activate(&manager, 2, &limited(3_600_000, 1));
    let next = candidate(&manager, 3, &limited(3_600_000, 1));

    assert!(manager.defer_for_retired_cap(&next));
    assert!(manager.defer_for_retired_cap(&next));
    let pending = manager.status().pending_apply.expect("pending apply");
    assert_eq!(pending.revision, next.revision().candidate);
    assert_eq!(pending.reason, "retired_cap");
    assert_eq!(
        manager.retirement_counters().cap_deferrals,
        1,
        "once per revision"
    );

    drop(request);
    assert!(!manager.defer_for_retired_cap(&next));
    assert!(manager.status().pending_apply.is_none());
}

#[test]
fn the_cap_comes_from_the_candidate_and_defaults_to_four() {
    let manager = GenerationManager::new();
    let mut held = Vec::new();
    for sequence in 1..=4 {
        let generation = activate(&manager, sequence, &config(None));
        held.push(hold(&generation, RuntimeReferenceKind::Http1));
    }
    let next = candidate(&manager, 5, &config(None));
    assert!(!manager.defer_for_retired_cap(&next), "three are retired");
    activate(&manager, 5, &config(None));
    let next = candidate(&manager, 6, &config(None));
    assert!(manager.defer_for_retired_cap(&next), "four are retired");
    let raised = candidate(&manager, 7, &limited(3_600_000, 5));
    assert!(!manager.defer_for_retired_cap(&raised));
}
