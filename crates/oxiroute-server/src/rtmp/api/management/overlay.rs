#[allow(clippy::wildcard_imports)]
use super::*;
use crate::{
    ServerAdminState,
    overlay::{OverlayEdit, OverlayStore, ServerEntry, Setting},
};

type Targets<'a> = &'a [(Arc<crate::RoundRobinPool>, String)];

impl ManagementState {
    pub(super) fn overlay_show(&self) -> ApiResponse {
        let store = match self.overlay_store() {
            Ok(store) => store,
            Err(response) => return response,
        };
        let path = store.path().display().to_string();
        let (Ok(overlay), Ok(revision)) = (store.load(), store.revision()) else {
            return dto_response(200, &OverlayResponse::unreadable(path));
        };
        let active = self.generations.active();
        let runtime = active
            .as_ref()
            .map(|active| active.administrative_states())
            .unwrap_or_default();
        let listeners = self.metrics.public_listener_snapshots().unwrap_or_default();
        let current = |entry: &ServerEntry| {
            let known = active
                .as_ref()
                .and_then(|active| find_pool(active, &entry.pool))
                .is_some_and(|pool| pool.has_server(&entry.server));
            known.then(|| {
                runtime
                    .iter()
                    .find(|state| state.pool == entry.pool && state.server == entry.server)
                    .cloned()
                    .unwrap_or_else(|| ServerAdminState::unset(&entry.pool, &entry.server))
            })
        };
        let mut settings = overlay
            .servers
            .iter()
            .flat_map(|entry| server_settings(entry, current(entry).as_ref()))
            .collect::<Vec<_>>();
        settings.extend(overlay.listeners.iter().map(|entry| {
            let current = listeners
                .iter()
                .find(|listener| listener.name == entry.listener)
                .map(|listener| listener.administrative_state);
            OverlaySetting {
                target: entry.listener.clone(),
                setting: "listener_state",
                value: json_name(&entry.state),
                status: status_of(current.map(|state| state == entry.state)),
            }
        }));
        dto_response(200, &OverlayResponse::of(path, revision, settings))
    }

    pub(super) fn overlay_clear(&self) -> ApiResponse {
        let store = match self.overlay_store() {
            Ok(store) => store,
            Err(response) => return response,
        };
        let removed = store.load().map_or(0, |overlay| overlay.settings());
        if store.clear().is_err() {
            return overlay_unavailable();
        }
        mutation_response("overlay_clear", removed, Persistence::Overlay)
    }

    pub(super) fn persist_servers(
        &self,
        persist: bool,
        targets: Targets<'_>,
        setting: Setting,
    ) -> Result<(), ApiResponse> {
        let edits = targets
            .iter()
            .filter(|(pool, server)| self.persisted(persist, pool, server, setting))
            .map(|(pool, server)| OverlayEdit::Server {
                pool: pool.name().to_owned(),
                server: server.clone(),
                setting,
            })
            .collect::<Vec<_>>();
        self.persist_edits(edits)
    }

    pub(super) fn persist_pools(
        &self,
        persist: bool,
        pools: &[Arc<crate::RoundRobinPool>],
        setting: Setting,
    ) -> Result<(), ApiResponse> {
        let targets = pools
            .iter()
            .flat_map(|pool| {
                pool.server_names()
                    .into_iter()
                    .map(|server| (Arc::clone(pool), server))
            })
            .collect::<Vec<_>>();
        self.persist_servers(persist, &targets, setting)
    }

    pub(super) fn persist_edits(&self, edits: Vec<OverlayEdit>) -> Result<(), ApiResponse> {
        if edits.is_empty() {
            return Ok(());
        }
        self.overlay_store()?
            .edit(edits)
            .map(drop)
            .map_err(|_| overlay_unavailable())
    }

    pub(super) fn persisted(
        &self,
        persist: bool,
        pool: &Arc<crate::RoundRobinPool>,
        server: &str,
        setting: Setting,
    ) -> bool {
        persist
            && !(matches!(setting, Setting::State(_))
                && self.registry_owner(pool, server).is_some())
    }

    pub(super) fn registry_owner(
        &self,
        pool: &Arc<crate::RoundRobinPool>,
        server: &str,
    ) -> Option<Arc<crate::registry::service::RegistryService>> {
        match pool.member(server) {
            Some((crate::ServerOrigin::Dynamic, _)) => self.generations.registry(),
            _ => None,
        }
    }

    fn overlay_store(&self) -> Result<&OverlayStore, ApiResponse> {
        self.generations.overlay().ok_or_else(|| {
            ApiResponse::error(
                503,
                "overlay_unavailable",
                "this process keeps no overlay beside a configuration file",
            )
        })
    }
}

fn overlay_unavailable() -> ApiResponse {
    ApiResponse::error(
        500,
        "overlay_unavailable",
        "the overlay file could not be read or written; fix or clear it",
    )
}

fn json_name(value: &impl serde::Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn status_of(applied: Option<bool>) -> OverlayStatus {
    match applied {
        None => OverlayStatus::Orphan,
        Some(true) => OverlayStatus::Applied,
        Some(false) => OverlayStatus::Diverged,
    }
}

fn server_settings(entry: &ServerEntry, runtime: Option<&ServerAdminState>) -> Vec<OverlaySetting> {
    let target = format!("{}/{}", entry.pool, entry.server);
    let row =
        |setting: &'static str, value: String, applies: &dyn Fn(&ServerAdminState) -> bool| {
            OverlaySetting {
                target: target.clone(),
                setting,
                value,
                status: status_of(runtime.map(applies)),
            }
        };
    let mut rows = Vec::new();
    if let Some(state) = entry.state {
        rows.push(row("state", json_name(&state), &|now| {
            now.administrative == state
        }));
    }
    if let Some(health) = entry.health {
        rows.push(row("health", json_name(&health), &|now| {
            now.health == health
        }));
    }
    if let Some(enabled) = entry.checks_enabled {
        let value = if enabled { "enabled" } else { "disabled" };
        rows.push(row("checks", value.into(), &|now| {
            now.checks_enabled == enabled
        }));
    }
    if let Some(limit) = entry.max_connections {
        rows.push(row("max_connections", limit.to_string(), &|now| {
            now.max_connections == Some(limit)
        }));
    }
    if let Some(weight) = entry.weight {
        rows.push(row("weight", weight.to_string(), &|now| {
            now.weight == Some(weight)
        }));
    }
    rows
}
