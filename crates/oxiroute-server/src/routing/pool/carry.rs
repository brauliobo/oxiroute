#[allow(clippy::wildcard_imports)]
use super::*;
use crate::routing::carry::PoolHealthCarry;

impl EndpointPool {
    #[must_use]
    pub fn health_carry(&self) -> PoolHealthCarry {
        PoolHealthCarry {
            pool: self.name.clone(),
            servers: self
                .members()
                .endpoints
                .iter()
                .map(|server| server.health_carry())
                .collect(),
        }
    }

    pub fn restore_health(&self, carried: &[PoolHealthCarry]) {
        let mut changed = false;
        for carry in carried.iter().filter(|carry| carry.pool == self.name) {
            for state in &carry.servers {
                let Some(server) = self.server_endpoint(&state.server) else {
                    continue;
                };
                changed |= self.health.restore(&server, state);
            }
        }
        if changed {
            self.health.selectable_changed();
            self.queue.notify_capacity_waiters();
        }
    }
}
