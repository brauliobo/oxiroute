use oxiroute_config::UpstreamPool;

use super::Overlay;

impl Overlay {
    pub(crate) fn promote_capacities(&self, pools: &mut [UpstreamPool]) -> Vec<(String, String)> {
        let mut promoted = Vec::new();
        for entry in self
            .servers
            .iter()
            .filter(|entry| entry.max_connections.is_some())
        {
            let configured = pools
                .iter_mut()
                .filter(|pool| pool.name == entry.pool)
                .flat_map(|pool| pool.servers.iter_mut())
                .find(|server| server.name == entry.server);
            if let Some(server) = configured {
                server.max_connections = entry.max_connections;
                promoted.push((entry.pool.clone(), entry.server.clone()));
            }
        }
        promoted
    }
}
