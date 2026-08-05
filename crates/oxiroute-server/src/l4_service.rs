use std::sync::Arc;

use oxiroute_config::{ProxyProtocolPolicy, UdpPolicy};

use crate::{EndpointLease, RelayPolicy, RoundRobinPool};

#[derive(Debug)]
pub struct L4ServicePlan {
    policy: RelayPolicy,
    pool: Arc<RoundRobinPool>,
    proxy_protocol: Option<ProxyProtocolPolicy>,
    udp: UdpPolicy,
}

impl L4ServicePlan {
    pub(crate) const fn new(
        policy: RelayPolicy,
        pool: Arc<RoundRobinPool>,
        proxy_protocol: Option<ProxyProtocolPolicy>,
        udp: UdpPolicy,
    ) -> Self {
        Self {
            policy,
            pool,
            proxy_protocol,
            udp,
        }
    }

    #[must_use]
    pub const fn policy(&self) -> RelayPolicy {
        self.policy
    }

    #[must_use]
    pub const fn udp_policy(&self) -> UdpPolicy {
        self.udp
    }

    #[must_use]
    pub const fn proxy_protocol(&self) -> Option<ProxyProtocolPolicy> {
        self.proxy_protocol
    }

    #[must_use]
    pub fn select(&self) -> Option<EndpointLease> {
        self.pool.select()
    }

    pub async fn select_wait(&self) -> Option<EndpointLease> {
        self.pool.select_wait().await
    }
}
