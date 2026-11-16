use std::sync::Arc;

use pingora::protocols::ConnectionLifetime;

use crate::EndpointLease;

pub(super) struct EngineLease {
    _lease: EndpointLease,
    lifetime: Arc<dyn ConnectionLifetime>,
}

impl EngineLease {
    pub(super) fn new(lease: EndpointLease) -> Option<Self> {
        let lifetime = lease.connection_lifetime().upgrade()?;
        Some(Self {
            _lease: lease,
            lifetime,
        })
    }
}

impl oxiroute_epoll_h1::Lease for EngineLease {
    fn connecting(&self) -> bool {
        self.lifetime.try_acquire().unwrap_or(false)
    }

    fn idle(&self, idle: bool) {
        if idle {
            self.lifetime.notify_reusable();
        } else {
            self.lifetime.notify_in_use();
        }
    }
}
