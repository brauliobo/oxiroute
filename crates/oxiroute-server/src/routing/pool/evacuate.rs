#[allow(clippy::wildcard_imports)]
use super::*;
use crate::{EvacuationReport, http::inflight, observability::upstream_stats::upstream_stats};

impl EndpointPool {
    /// Drains `name` like `set_server_administrative_state` does, then moves the HTTP requests running
    /// on it to other servers of this pool (or kills the ones that cannot move when `force`).
    ///
    /// # Errors
    ///
    /// Returns an error when this pool has no server with the exact canonical name.
    pub fn evacuate_server(
        &self,
        name: &str,
        force: bool,
    ) -> Result<EvacuationReport, PoolAdminError> {
        self.set_server_administrative_state(name, AdministrativeState::Drain)?;
        self.cancel_server_requests(name, force)
    }

    /// The cancel half of [`Self::evacuate_server`], for a server whose drain was already applied
    /// elsewhere, such as through the registry operator state.
    ///
    /// # Errors
    ///
    /// Returns an error when this pool has no server with the exact canonical name.
    pub fn cancel_server_requests(
        &self,
        name: &str,
        force: bool,
    ) -> Result<EvacuationReport, PoolAdminError> {
        self.server(name)?;
        let report = inflight::registry().evacuate_server(
            &self.name,
            name,
            force,
            self.has_destination(name),
        );
        upstream_stats()
            .server(&self.name, name)
            .record_evacuation_report(&report);
        Ok(report)
    }

    /// Whether a request cancelled on `evacuated` would find another server to run on: one that can
    /// be selected (or failing open) and either has capacity or can queue the request.
    fn has_destination(&self, evacuated: &str) -> bool {
        let now_unix_ms = now_unix_ms();
        let failing_open = self.failing_open(now_unix_ms);
        self.read_health(|members| {
            members.endpoints.iter().any(|server| {
                server.name != evacuated
                    && server.admission(now_unix_ms, failing_open).is_some()
                    && (self.queue_timeout.is_some() || server.has_capacity())
            })
        })
    }
}
