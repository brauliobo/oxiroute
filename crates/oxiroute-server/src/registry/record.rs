use std::{collections::BTreeMap, net::SocketAddr};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::AdministrativeState;

/// What the worker itself announces; `draining` is set before the worker stops.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AnnouncedState {
    #[default]
    Ready,
    Draining,
}

/// One registered server as persisted. The lease deadline is deliberately not part of it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Registration {
    pub pool: String,
    pub name: String,
    pub address: SocketAddr,
    pub weight: u16,
    pub priority: u16,
    pub max_connections: Option<u64>,
    pub labels: BTreeMap<String, String>,
    pub metadata: serde_json::Value,
    pub ttl_ms: u64,
    pub announced_state: AnnouncedState,
    pub operator_state: AdministrativeState,
    pub registered_by: String,
}

impl Registration {
    pub fn key(&self) -> RegistrationKey {
        RegistrationKey::new(&self.pool, &self.name)
    }

    /// The stricter of the worker's announcement and the operator's decision.
    pub fn effective_state(&self) -> AdministrativeState {
        match (self.operator_state, self.announced_state) {
            (AdministrativeState::Ready, AnnouncedState::Draining) => AdministrativeState::Drain,
            (state, _) => state,
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RegistrationKey {
    pub pool: String,
    pub name: String,
}

impl RegistrationKey {
    pub fn new(pool: &str, name: &str) -> Self {
        Self {
            pool: pool.to_owned(),
            name: name.to_owned(),
        }
    }
}

/// The body of `PUT /pools/{pool}/servers/{name}`.
#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RegistrationRequest {
    pub address: Option<SocketAddr>,
    pub weight: Option<u16>,
    pub priority: Option<u16>,
    pub max_connections: Option<u64>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    #[serde(default)]
    pub metadata: serde_json::Value,
    pub ttl_ms: Option<u64>,
    #[serde(default)]
    pub state: AnnouncedState,
}

/// The optional body of a heartbeat.
#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct HeartbeatRequest {
    pub state: Option<AnnouncedState>,
    pub labels: Option<BTreeMap<String, String>>,
    pub metadata: Option<serde_json::Value>,
    pub ttl_ms: Option<u64>,
}
