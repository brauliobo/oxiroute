use oxiroute_config::{UpstreamAlgorithm, UpstreamPool};

use super::Overlay;

/// Settings promoted from the runtime overlay into the canonical configuration draft.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct PromotedSettings {
    pub(crate) capacities: Vec<(String, String)>,
    pub(crate) weights: Vec<(String, String)>,
}

impl PromotedSettings {
    pub(crate) fn is_empty(&self) -> bool {
        self.capacities.is_empty() && self.weights.is_empty()
    }
}

impl Overlay {
    /// Promotes persisted static server capacities (`max_connections`) and `weighted_round_robin`
    /// server weights into `pools`. Operational states (`drain`, `maintenance`, health overrides,
    /// check toggles) are never promoted to configuration files.
    pub(crate) fn promote_to_draft(&self, pools: &mut [UpstreamPool]) -> PromotedSettings {
        let mut promoted = PromotedSettings::default();
        for entry in &self.servers {
            let Some(pool) = pools.iter_mut().find(|pool| pool.name == entry.pool) else {
                continue;
            };
            let Some(index) = pool
                .servers
                .iter()
                .position(|server| server.name == entry.server)
            else {
                continue;
            };
            if let Some(limit) = entry.max_connections {
                pool.servers[index].max_connections = Some(limit);
                promoted
                    .capacities
                    .push((entry.pool.clone(), entry.server.clone()));
            }
            if let Some(weight) = entry.weight
                && let UpstreamAlgorithm::WeightedRoundRobin { weights } = &mut pool.algorithm
                && index < weights.len()
            {
                weights[index] = weight;
                promoted
                    .weights
                    .push((entry.pool.clone(), entry.server.clone()));
            }
        }
        promoted
    }
}
