use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{AdministrativeState, HealthOverride, ServerAdminState};

pub(super) const SCHEMA_VERSION: u32 = 1;
pub(super) const MAX_SERVERS: usize = 4_096;
pub(super) const MAX_LISTENERS: usize = 256;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Overlay {
    schema_version: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) servers: Vec<ServerEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) listeners: Vec<ListenerEntry>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ServerEntry {
    pub(crate) pool: String,
    pub(crate) server: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) state: Option<AdministrativeState>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) health: Option<HealthOverride>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) checks_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) max_connections: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) weight: Option<u16>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ListenerEntry {
    pub(crate) listener: String,
    pub(crate) state: AdministrativeState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Setting {
    State(AdministrativeState),
    Health(HealthOverride),
    Checks(bool),
    MaxConnections(Option<u64>),
    Weight(Option<u16>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum OverlayEdit {
    Server {
        pool: String,
        server: String,
        setting: Setting,
    },
    Listener {
        listener: String,
        state: AdministrativeState,
    },
}

impl Default for Overlay {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            servers: Vec::new(),
            listeners: Vec::new(),
        }
    }
}

impl ServerEntry {
    fn named(pool: &str, server: &str) -> Self {
        Self {
            pool: pool.to_owned(),
            server: server.to_owned(),
            ..Self::default()
        }
    }

    fn set(&mut self, setting: Setting) {
        match setting {
            Setting::State(state) => self.state = Some(state),
            Setting::Health(health) => self.health = Some(health),
            Setting::Checks(enabled) => self.checks_enabled = Some(enabled),
            Setting::MaxConnections(limit) => self.max_connections = limit,
            Setting::Weight(weight) => self.weight = weight,
        }
        self.prune_defaults();
    }

    fn prune_defaults(&mut self) {
        self.state = self
            .state
            .filter(|state| *state != AdministrativeState::Ready);
        self.health = self.health.filter(|health| *health != HealthOverride::Auto);
        self.checks_enabled = self.checks_enabled.filter(|enabled| !enabled);
    }

    pub(crate) fn settings(&self) -> usize {
        usize::from(self.state.is_some())
            + usize::from(self.health.is_some())
            + usize::from(self.checks_enabled.is_some())
            + usize::from(self.max_connections.is_some())
            + usize::from(self.weight.is_some())
    }

    const fn is_empty(&self) -> bool {
        self.state.is_none()
            && self.health.is_none()
            && self.checks_enabled.is_none()
            && self.max_connections.is_none()
            && self.weight.is_none()
    }

    fn fill(&self, runtime: &mut ServerAdminState) {
        if runtime.administrative == AdministrativeState::Ready {
            runtime.administrative = self.state.unwrap_or_default();
        }
        if runtime.health == HealthOverride::Auto {
            runtime.health = self.health.unwrap_or_default();
        }
        runtime.checks_enabled &= self.checks_enabled.unwrap_or(true);
        runtime.max_connections = runtime.max_connections.or(self.max_connections);
        runtime.weight = runtime.weight.or(self.weight);
    }
}

impl Overlay {
    pub(crate) fn is_empty(&self) -> bool {
        self.servers.is_empty() && self.listeners.is_empty()
    }

    pub(crate) fn settings(&self) -> usize {
        self.servers
            .iter()
            .map(ServerEntry::settings)
            .sum::<usize>()
            + self.listeners.len()
    }

    pub(crate) fn edit(&mut self, edit: OverlayEdit) {
        match edit {
            OverlayEdit::Server {
                pool,
                server,
                setting,
            } => self.entry(&pool, &server).set(setting),
            OverlayEdit::Listener { listener, state } => {
                self.listeners.retain(|entry| entry.listener != listener);
                self.listeners.push(ListenerEntry { listener, state });
            }
        }
        self.normalize();
    }

    pub(crate) fn edited(mut self, edits: impl IntoIterator<Item = OverlayEdit>) -> Self {
        for edit in edits {
            self.edit(edit);
        }
        self
    }

    fn entry(&mut self, pool: &str, server: &str) -> &mut ServerEntry {
        let position = self
            .servers
            .iter()
            .position(|entry| entry.pool == pool && entry.server == server)
            .unwrap_or_else(|| {
                self.servers.push(ServerEntry::named(pool, server));
                self.servers.len() - 1
            });
        &mut self.servers[position]
    }

    pub(crate) fn merged_with(&self, carried: &[ServerAdminState]) -> Vec<ServerAdminState> {
        let mut merged = carried
            .iter()
            .map(|state| ((state.pool.clone(), state.server.clone()), state.clone()))
            .collect::<BTreeMap<_, _>>();
        for entry in &self.servers {
            let runtime = merged
                .entry((entry.pool.clone(), entry.server.clone()))
                .or_insert_with(|| ServerAdminState::unset(&entry.pool, &entry.server));
            entry.fill(runtime);
        }
        merged.into_values().collect()
    }

    #[cfg(test)]
    pub(crate) fn without_max_connections(self, covered: &[(String, String)]) -> Self {
        self.without_promoted(covered, &[])
    }

    pub(crate) fn without_promoted(
        mut self,
        capacities: &[(String, String)],
        weights: &[(String, String)],
    ) -> Self {
        for entry in &mut self.servers {
            let key = (entry.pool.clone(), entry.server.clone());
            if capacities.contains(&key) {
                entry.max_connections = None;
            }
            if weights.contains(&key) {
                entry.weight = None;
            }
        }
        self.normalize();
        self
    }

    pub(super) fn within_bounds(&self) -> bool {
        self.servers.len() <= MAX_SERVERS && self.listeners.len() <= MAX_LISTENERS
    }

    pub(super) const fn has_known_schema(&self) -> bool {
        self.schema_version == SCHEMA_VERSION
    }

    pub(super) fn normalize(&mut self) {
        for entry in &mut self.servers {
            entry.prune_defaults();
        }
        self.servers.retain(|entry| !entry.is_empty());
        self.servers
            .sort_by(|left, right| (&left.pool, &left.server).cmp(&(&right.pool, &right.server)));
        self.listeners
            .retain(|entry| entry.state != AdministrativeState::Ready);
        self.listeners
            .sort_by(|left, right| left.listener.cmp(&right.listener));
    }
}
