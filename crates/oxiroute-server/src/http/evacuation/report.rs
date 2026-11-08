use std::collections::BTreeMap;

use serde::Serialize;

use super::Fate;

/// Why a request keeps running on an evacuated server. Bounded, so it is safe as a label and a JSON key.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LeftRunning {
    ResponseStarted,
    NotReplayable,
    BodyNotReplayable,
    Upgraded,
    LimitReached,
    NoDestination,
}

impl LeftRunning {
    pub(super) const ALL: [Self; 6] = [
        Self::ResponseStarted,
        Self::NotReplayable,
        Self::BodyNotReplayable,
        Self::Upgraded,
        Self::LimitReached,
        Self::NoDestination,
    ];

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ResponseStarted => "response_started",
            Self::NotReplayable => "not_replayable",
            Self::BodyNotReplayable => "body_not_replayable",
            Self::Upgraded => "upgraded",
            Self::LimitReached => "limit_reached",
            Self::NoDestination => "no_destination",
        }
    }

    /// Why a request can never be moved, from what its route and protocol allow.
    pub(crate) const fn unmovable(upgrade: bool, replay_safe: bool) -> Option<Self> {
        match (upgrade, replay_safe) {
            (true, _) => Some(Self::Upgraded),
            (false, false) => Some(Self::NotReplayable),
            (false, true) => None,
        }
    }
}

/// What an evacuation did to the requests running on one or more servers.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize)]
pub struct EvacuationReport {
    #[serde(rename = "cancelledAndRedispatched")]
    pub redispatched: u64,
    pub forced: u64,
    #[serde(rename = "leftRunning")]
    pub left_running: BTreeMap<LeftRunning, u64>,
}

impl EvacuationReport {
    pub(crate) fn record(&mut self, fate: Fate) {
        match fate {
            Fate::Redispatched => self.redispatched += 1,
            Fate::Forced => self.forced += 1,
            Fate::LeftRunning(reason) => *self.left_running.entry(reason).or_default() += 1,
        }
    }

    pub fn merge(&mut self, other: Self) {
        self.redispatched += other.redispatched;
        self.forced += other.forced;
        for (reason, count) in other.left_running {
            *self.left_running.entry(reason).or_default() += count;
        }
    }

    #[must_use]
    pub fn left_running_total(&self) -> u64 {
        self.left_running.values().sum()
    }
}
