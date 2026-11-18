use std::{collections::HashMap, sync::Arc};

use oxiroute_cache::Cache;

use crate::http::cache::HttpCacheBackend;

#[derive(Clone, Default)]
pub struct WarmCaches {
    memory: Vec<(String, Arc<Cache>)>,
}

impl WarmCaches {
    pub(crate) fn from_backends(backends: &HashMap<String, Arc<HttpCacheBackend>>) -> Self {
        let mut memory = backends
            .iter()
            .filter_map(|(name, backend)| match &**backend {
                HttpCacheBackend::Memory(cache) => Some((name.clone(), Arc::clone(cache))),
                HttpCacheBackend::Disk(_) => None,
            })
            .collect::<Vec<_>>();
        memory.sort_by(|left, right| left.0.cmp(&right.0));
        Self { memory }
    }

    pub fn inherit(&self, previous: &Self) {
        for (name, cache) in &self.memory {
            let Some((_, replaced)) = previous.memory.iter().find(|(other, _)| other == name)
            else {
                continue;
            };
            match cache.inherit(replaced) {
                Ok(inherited) => log::info!(
                    "memory cache store `{name}` kept {} of {} entries across the reload",
                    inherited.kept,
                    inherited.offered
                ),
                Err(error) => log::error!("memory cache store `{name}` starts empty: {error}"),
            }
        }
    }
}
