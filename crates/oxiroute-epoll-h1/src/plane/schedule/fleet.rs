use std::{io, sync::Arc};

use oxiroute_aged_order::Age;
use oxiroute_worker_scaling::{Policy, Scaling, Workers};

use super::{
    peers::{EngineWorkers, Peers},
    placement::Placement,
};
use crate::plane::{
    config::{Config, Driver},
    wake::Waker,
};

#[derive(Clone, Debug)]
pub struct Scale {
    pub name: String,
    pub policy: Policy,
}

#[derive(Clone, Debug)]
pub struct Scheduling {
    pub age: Age,
    pub scale: Option<Scale>,
}

pub(crate) struct Plan<A> {
    pub(crate) placement: Placement<A>,
    pub(crate) age: Option<Age>,
}

pub(crate) struct Fleet<A> {
    peers: Arc<Peers<A>>,
    scaling: Option<Arc<Scaling>>,
    age: Option<Age>,
}

impl<A: Send + 'static> Fleet<A> {
    pub(crate) fn of(config: &Config, wakes: Vec<Waker>) -> io::Result<Option<Self>> {
        let balances = config.driver == Driver::Epoll && wakes.len() > 1;
        if config.scheduling.is_none() && !balances {
            return Ok(None);
        }
        let threads = wakes.len();
        let scaling = config
            .scheduling
            .as_ref()
            .and_then(|scheduling| scheduling.scale.as_ref())
            .filter(|scale| threads > scale.policy.min_active)
            .map(|scale| Arc::new(Scaling::new(&scale.name, scale.policy, threads)));
        Ok(Some(Self {
            peers: Arc::new(Peers::new(wakes)?),
            scaling,
            age: config.scheduling.as_ref().map(|scheduling| scheduling.age),
        }))
    }

    pub(crate) fn plan(&self, thread: usize) -> Plan<A> {
        Plan {
            placement: Placement::new(Arc::clone(&self.peers), self.scaling.clone(), thread),
            age: self.age,
        }
    }

    pub(crate) fn start_scaling(&self) {
        if let Some(scaling) = &self.scaling {
            scaling.start(self.workers());
        }
    }

    pub(crate) fn scaling(&self) -> Option<Arc<Scaling>> {
        self.scaling.clone()
    }

    pub(crate) fn workers(&self) -> Arc<dyn Workers> {
        Arc::new(EngineWorkers(Arc::clone(&self.peers)))
    }
}
