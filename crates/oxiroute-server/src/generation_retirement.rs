//! Replaced generations that keep serving the work they already admitted.
//!
//! A generation retires when its replacement is published: its accept gate is closed and in-flight
//! work continues until it ends or the generation's drain deadline passes.

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};

use log::info;
use serde::{Deserialize, Serialize, Serializer};

use crate::{
    RuntimeGeneration,
    config_coordinator::{ConfigRejection, EffectiveRevision},
    operational_event,
};

/// A generation that was asked to close stays listed this long while its teardown finishes.
const CLOSE_GRACE: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetiredState {
    Draining,
    Closing,
}

impl RetiredState {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Draining => "draining",
            Self::Closing => "closing",
        }
    }
}

impl Serialize for RetiredState {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RetiredGenerationStatus {
    pub revision: EffectiveRevision,
    pub state: RetiredState,
    pub age_seconds: u64,
    pub deadline_remaining_seconds: u64,
    pub in_flight: u64,
    pub in_flight_by_kind: BTreeMap<&'static str, u64>,
}

/// What the supervising master tells its active worker about the worker it replaced.
#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "notice", rename_all = "snake_case")]
pub enum WorkerNotice {
    /// The newest configuration the master refused, or `None` once a file was accepted.
    Config { rejection: Option<ConfigRejection> },
    /// The retired workers still draining; an empty list once none is left.
    Retired { generations: Vec<RemoteRetired> },
}

/// A replaced generation that drains in another process, as the master last observed it.
///
/// The master sees the retired worker's connection total, not its work by protocol.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct RemoteRetired {
    pub revision: String,
    pub age_seconds: u64,
    pub deadline_remaining_seconds: u64,
    pub connections: u64,
}

impl RemoteRetired {
    /// Lists this report as it stands `elapsed` after the worker received it.
    pub(crate) fn status(
        &self,
        revision: &EffectiveRevision,
        elapsed: Duration,
    ) -> RetiredGenerationStatus {
        RetiredGenerationStatus {
            revision: revision.clone(),
            state: RetiredState::Draining,
            age_seconds: self.age_seconds + elapsed.as_secs(),
            deadline_remaining_seconds: self
                .deadline_remaining_seconds
                .saturating_sub(elapsed.as_secs()),
            in_flight: self.connections,
            in_flight_by_kind: BTreeMap::from([("connections", self.connections)]),
        }
    }
}

/// A prepared revision that is waiting for a retired generation to finish.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct PendingApply {
    pub revision: EffectiveRevision,
    pub reason: &'static str,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct RetirementCounters {
    pub forced_close_drain_deadline: u64,
    pub forced_close_shutdown: u64,
    pub cap_deferrals: u64,
}

struct Retired {
    generation: Arc<RuntimeGeneration>,
    since: Instant,
    deadline: Duration,
    closed_at: Option<Instant>,
}

impl Retired {
    fn due(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.since) >= self.deadline
    }

    fn status(&self, now: Instant) -> RetiredGenerationStatus {
        let age = now.saturating_duration_since(self.since);
        RetiredGenerationStatus {
            revision: self.generation.revision().candidate.clone(),
            state: if self.closed_at.is_some() {
                RetiredState::Closing
            } else {
                RetiredState::Draining
            },
            age_seconds: age.as_secs(),
            deadline_remaining_seconds: self.deadline.saturating_sub(age).as_secs(),
            in_flight: self.generation.in_flight(),
            in_flight_by_kind: self.generation.in_flight_by_kind(),
        }
    }
}

/// Oldest first, as retired.
#[derive(Default)]
pub(crate) struct RetiredGenerations {
    entries: Vec<Retired>,
    counters: RetirementCounters,
    pending: Option<PendingApply>,
}

impl RetiredGenerations {
    pub(crate) fn retire(&mut self, generation: Arc<RuntimeGeneration>, now: Instant) {
        generation.mark_retired();
        let deadline = Duration::from_millis(generation.config().retirement_deadline_ms());
        operational_event::emit(
            "generation_retire",
            "retiring",
            Some(&generation.revision().candidate),
        );
        self.entries.push(Retired {
            generation,
            since: now,
            deadline,
            closed_at: None,
        });
    }

    /// Forgets generations that finished their work, or that were told to close and did not end.
    pub(crate) fn sweep_drained(&mut self, now: Instant) {
        self.entries.retain(|entry| {
            let drained = entry.generation.drained();
            if drained {
                operational_event::emit(
                    "generation_retire",
                    "drained",
                    Some(&entry.generation.revision().candidate),
                );
            }
            !drained
                && entry
                    .closed_at
                    .is_none_or(|closed| now < closed + CLOSE_GRACE)
        });
    }

    /// Marks generations past their deadline as closing and returns them once, for the caller to
    /// tear down.
    pub(crate) fn expire_due(&mut self, now: Instant) -> Vec<Arc<RuntimeGeneration>> {
        let mut expired = Vec::new();
        for entry in &mut self.entries {
            if entry.closed_at.is_none() && entry.due(now) {
                entry.closed_at = Some(now);
                self.counters.forced_close_drain_deadline += 1;
                Self::announce_forced_close(entry, "drain_deadline");
                expired.push(Arc::clone(&entry.generation));
            }
        }
        expired
    }

    /// Process shutdown ends whatever is still draining.
    pub(crate) fn close_for_shutdown(&mut self) {
        for entry in &mut self.entries {
            if entry.closed_at.is_none() && !entry.generation.drained() {
                entry.closed_at = Some(Instant::now());
                self.counters.forced_close_shutdown += 1;
                Self::announce_forced_close(entry, "shutdown");
            }
        }
    }

    fn announce_forced_close(entry: &Retired, reason: &str) {
        entry.generation.announce_forced_close(reason);
    }

    pub(crate) fn statuses(&self, now: Instant) -> Vec<RetiredGenerationStatus> {
        self.entries.iter().map(|entry| entry.status(now)).collect()
    }

    /// Whether a revision must wait because `limit` generations are still draining.
    pub(crate) fn defer(&mut self, revision: &EffectiveRevision, limit: usize) -> bool {
        if self.entries.len() < limit {
            self.pending = None;
            return false;
        }
        if self
            .pending
            .as_ref()
            .is_none_or(|pending| &pending.revision != revision)
        {
            self.counters.cap_deferrals += 1;
            info!(
                "revision {} waits for one of {limit} retired generations to finish",
                revision.as_str()
            );
            operational_event::emit("generation_retire", "cap_reached", Some(revision));
            self.pending = Some(PendingApply {
                revision: revision.clone(),
                reason: "retired_cap",
            });
        }
        true
    }

    pub(crate) fn pending(&self) -> Option<&PendingApply> {
        self.pending.as_ref()
    }

    pub(crate) const fn counters(&self) -> RetirementCounters {
        self.counters
    }
}
