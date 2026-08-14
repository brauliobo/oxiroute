use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock},
    time::Instant,
};

use oxiroute_rtmp::{
    MediaCatalog, RtmpAutoPushStatus, RtmpRecorderLifecycle, RtmpRecorderShutdown, RtmpRegistry,
    RtmpServiceRuntime, VodCatalog,
};

use crate::{GenerationError, service_plan::PreparedRtmpRuntime};

pub(crate) struct PreparedRtmpGenerationRuntime {
    media_catalog: Arc<MediaCatalog>,
    prepared: Vec<PreparedRtmpRuntime>,
    registry: Arc<RtmpRegistry>,
    vod_catalog: Arc<VodCatalog>,
}

pub(crate) struct RtmpGenerationRuntime {
    media_catalog: Arc<MediaCatalog>,
    prepared: Mutex<Option<Vec<PreparedRtmpRuntime>>>,
    registry: Arc<RtmpRegistry>,
    runtimes: OnceLock<StartedRtmpRuntimes>,
    vod_catalog: Arc<VodCatalog>,
}

pub(crate) struct StartedRtmpGenerationRuntime {
    runtimes: StartedRtmpRuntimes,
}

#[derive(Clone)]
pub(crate) struct RtmpRetirement {
    lifecycles: Vec<RtmpRecorderLifecycle>,
}

#[derive(Default)]
pub(crate) struct RtmpRetirementRegistry {
    retirements: Vec<RetiredRtmpLifecycle>,
}

struct RetiredRtmpLifecycle {
    identity: RtmpRecorderLifecycle,
    authority: RtmpRetirementAuthority,
    initiating: bool,
}

#[derive(Clone)]
enum RtmpRetirementAuthority {
    Lifecycle(RtmpRecorderLifecycle),
    Shutdown(RtmpRecorderShutdown),
}

pub(crate) struct RtmpRetirementWork {
    identity: RtmpRecorderLifecycle,
    authority: RtmpRetirementAuthority,
}

impl PreparedRtmpGenerationRuntime {
    pub(crate) fn new(
        registry: Arc<RtmpRegistry>,
        prepared: Vec<PreparedRtmpRuntime>,
        vod_catalog: Arc<VodCatalog>,
        media_catalog: Arc<MediaCatalog>,
    ) -> Self {
        Self {
            media_catalog,
            prepared,
            registry,
            vod_catalog,
        }
    }

    pub(crate) fn commit(self) -> RtmpGenerationRuntime {
        RtmpGenerationRuntime {
            media_catalog: self.media_catalog,
            prepared: Mutex::new(Some(self.prepared)),
            registry: self.registry,
            runtimes: OnceLock::new(),
            vod_catalog: self.vod_catalog,
        }
    }
}

impl RtmpGenerationRuntime {
    pub(crate) const fn registry(&self) -> &Arc<RtmpRegistry> {
        &self.registry
    }

    pub(crate) const fn vod_catalog(&self) -> &Arc<VodCatalog> {
        &self.vod_catalog
    }

    pub(crate) const fn media_catalog(&self) -> &Arc<MediaCatalog> {
        &self.media_catalog
    }

    pub(crate) fn runtime(&self, service: &str) -> Option<&RtmpServiceRuntime> {
        self.runtimes.get()?.get(service)
    }

    pub(crate) fn auto_push_status(&self) -> RtmpAutoPushStatus {
        self.runtimes
            .get()
            .into_iter()
            .flat_map(StartedRtmpRuntimes::values)
            .fold(RtmpAutoPushStatus::default(), |mut total, runtime| {
                let status = runtime.auto_push_status();
                total.enabled |= status.enabled;
                total.started |= status.started;
                total.peers = total.peers.saturating_add(status.peers);
                total.source_streams = total.source_streams.saturating_add(status.source_streams);
                total.remote_streams = total.remote_streams.saturating_add(status.remote_streams);
                total.frames_sent = total.frames_sent.saturating_add(status.frames_sent);
                total.frames_received =
                    total.frames_received.saturating_add(status.frames_received);
                total.frames_dropped = total.frames_dropped.saturating_add(status.frames_dropped);
                total.authentication_failures = total
                    .authentication_failures
                    .saturating_add(status.authentication_failures);
                total.reconnects = total.reconnects.saturating_add(status.reconnects);
                total.queue_messages = total.queue_messages.saturating_add(status.queue_messages);
                total.queue_bytes = total.queue_bytes.saturating_add(status.queue_bytes);
                total.last_failure = total.last_failure.or(status.last_failure);
                total
            })
    }

    pub(crate) fn close_admission(&self) {
        for runtime in self
            .runtimes
            .get()
            .into_iter()
            .flat_map(StartedRtmpRuntimes::values)
        {
            runtime.close_admission();
        }
    }

    pub(crate) fn initiate_recorder_shutdown(
        &self,
        deadline: Instant,
    ) -> Vec<RtmpRecorderShutdown> {
        self.runtimes
            .get()
            .into_iter()
            .flat_map(StartedRtmpRuntimes::values)
            .filter_map(|runtime| runtime.initiate_recorder_shutdown(deadline))
            .collect()
    }

    pub(crate) fn retirement(&self) -> RtmpRetirement {
        let mut lifecycles = Vec::new();
        for runtime in self
            .runtimes
            .get()
            .into_iter()
            .flat_map(StartedRtmpRuntimes::values)
        {
            runtime.close_admission();
            if let Some(lifecycle) = runtime.recorder_lifecycle()
                && !lifecycles
                    .iter()
                    .any(|existing: &RtmpRecorderLifecycle| existing.is_same_lifecycle(&lifecycle))
            {
                lifecycles.push(lifecycle);
            }
        }
        RtmpRetirement { lifecycles }
    }

    pub(crate) fn start_prepared(&self) -> Result<StartedRtmpGenerationRuntime, GenerationError> {
        let prepared = self
            .prepared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .unwrap_or_default();
        let mut started = Vec::with_capacity(prepared.len());
        for prepared in prepared {
            let service = prepared.service_id().to_owned();
            match prepared.start(Arc::clone(&self.registry)) {
                Ok(runtime) => started.push((service, runtime)),
                Err(error) => {
                    while let Some((service, runtime)) = started.pop() {
                        #[cfg(test)]
                        crate::service_plan::trace_rtmp_rollback(&service);
                        #[cfg(not(test))]
                        let _ = service;
                        drop(runtime);
                    }
                    return Err(GenerationError::Rtmp(error));
                }
            }
        }
        let runtimes = StartedRtmpRuntimes::new(started);
        Ok(StartedRtmpGenerationRuntime { runtimes })
    }

    pub(crate) fn install(
        &self,
        started: StartedRtmpGenerationRuntime,
    ) -> Result<(), GenerationError> {
        self.runtimes
            .set(started.runtimes)
            .map_err(|_| GenerationError::RuntimePrepare)
    }
}

impl RtmpRetirementRegistry {
    pub(crate) fn retire(&mut self, retirement: RtmpRetirement) {
        self.prune_completed();
        for lifecycle in retirement.lifecycles {
            if self
                .retirements
                .iter()
                .any(|retirement| retirement.identity.is_same_lifecycle(&lifecycle))
            {
                continue;
            }
            self.retirements.push(RetiredRtmpLifecycle {
                identity: lifecycle.clone(),
                authority: RtmpRetirementAuthority::Lifecycle(lifecycle),
                initiating: false,
            });
        }
    }

    pub(crate) fn take_shutdown_work(&mut self) -> Vec<RtmpRetirementWork> {
        let mut work = Vec::with_capacity(self.retirements.len());
        for retirement in &mut self.retirements {
            let identity = retirement.identity.clone();
            let authority = match &retirement.authority {
                RtmpRetirementAuthority::Lifecycle(lifecycle) if !retirement.initiating => {
                    retirement.initiating = true;
                    Some(RtmpRetirementAuthority::Lifecycle(lifecycle.clone()))
                }
                RtmpRetirementAuthority::Shutdown(shutdown) => {
                    Some(RtmpRetirementAuthority::Shutdown(shutdown.clone()))
                }
                RtmpRetirementAuthority::Lifecycle(_) => None,
            };
            if let Some(authority) = authority {
                work.push(RtmpRetirementWork {
                    identity,
                    authority,
                });
            }
        }
        work
    }

    pub(crate) fn store_shutdown(
        &mut self,
        identity: &RtmpRecorderLifecycle,
        shutdown: RtmpRecorderShutdown,
    ) {
        if let Some(retirement) = self
            .retirements
            .iter_mut()
            .find(|retirement| retirement.identity.is_same_lifecycle(identity))
        {
            retirement.authority = RtmpRetirementAuthority::Shutdown(shutdown);
            retirement.initiating = false;
        }
    }

    pub(crate) fn prune_completed(&mut self) {
        self.retirements
            .retain(|retirement| match &retirement.authority {
                RtmpRetirementAuthority::Shutdown(shutdown) => !shutdown.is_complete(),
                RtmpRetirementAuthority::Lifecycle(lifecycle) => !lifecycle.is_complete(),
            });
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.retirements.len()
    }
}

impl RtmpRetirementWork {
    pub(crate) fn initiate(
        self,
        deadline: Instant,
    ) -> (RtmpRecorderLifecycle, RtmpRecorderShutdown) {
        let shutdown = match self.authority {
            RtmpRetirementAuthority::Lifecycle(lifecycle) => lifecycle.initiate_shutdown(deadline),
            RtmpRetirementAuthority::Shutdown(shutdown) => shutdown,
        };
        (self.identity, shutdown)
    }
}

struct StartedRtmpRuntimes {
    ordered: Vec<(String, RtmpServiceRuntime)>,
    index: HashMap<String, usize>,
}

impl StartedRtmpRuntimes {
    fn new(ordered: Vec<(String, RtmpServiceRuntime)>) -> Self {
        let index = ordered
            .iter()
            .enumerate()
            .map(|(index, (service, _))| (service.clone(), index))
            .collect();
        Self { ordered, index }
    }

    fn get(&self, service: &str) -> Option<&RtmpServiceRuntime> {
        self.index
            .get(service)
            .and_then(|index| self.ordered.get(*index))
            .map(|(_, runtime)| runtime)
    }

    fn values(&self) -> impl Iterator<Item = &RtmpServiceRuntime> {
        self.ordered.iter().map(|(_, runtime)| runtime)
    }
}
