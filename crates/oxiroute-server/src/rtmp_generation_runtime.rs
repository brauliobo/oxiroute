use std::{
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};

use oxiroute_rtmp::{
    MediaCatalog, PreparedRtmpRuntimeSet, RtmpAutoPushStatus, RtmpControlHandle,
    RtmpRecorderLifecycle, RtmpRecorderShutdown, RtmpRegistry, RtmpRuntimeSet, RtmpRuntimeSetError,
    RtmpServiceHandle, VodCatalog,
};

use crate::GenerationError;

pub(crate) struct RtmpGenerationRuntime {
    prepared: Mutex<Option<PreparedRtmpRuntimeSet>>,
    registry: Arc<RtmpRegistry>,
    runtimes: OnceLock<RtmpRuntimeSet>,
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

impl RtmpGenerationRuntime {
    pub(crate) fn new(registry: Arc<RtmpRegistry>, prepared: PreparedRtmpRuntimeSet) -> Self {
        Self {
            prepared: Mutex::new(Some(prepared)),
            registry,
            runtimes: OnceLock::new(),
        }
    }

    pub(crate) fn control(&self) -> RtmpControlHandle {
        self.runtimes
            .get()
            .expect("started RTMP runtime set")
            .control()
    }

    pub(crate) fn vod_catalog(&self) -> Arc<VodCatalog> {
        self.control().vod_catalog()
    }

    pub(crate) fn media_catalog(&self) -> Arc<MediaCatalog> {
        self.control().media_catalog()
    }

    pub(crate) fn service(&self, service: &str) -> Option<RtmpServiceHandle> {
        self.runtimes.get()?.service(service)
    }

    pub(crate) fn auto_push_status(&self) -> RtmpAutoPushStatus {
        self.runtimes
            .get()
            .map_or_else(RtmpAutoPushStatus::default, |runtimes| {
                runtimes.control().auto_push_status()
            })
    }

    pub(crate) fn close_admission(&self) {
        if let Some(runtimes) = self.runtimes.get() {
            runtimes.control().close_admission();
        }
    }

    pub(crate) fn initiate_recorder_shutdown(
        &self,
        deadline: Instant,
    ) -> Vec<RtmpRecorderShutdown> {
        self.runtimes.get().map_or_else(Vec::new, |runtimes| {
            runtimes
                .recorder_lifecycles()
                .into_iter()
                .map(|lifecycle| lifecycle.initiate_shutdown(deadline))
                .collect()
        })
    }

    pub(crate) fn retirement(&self) -> RtmpRetirement {
        self.close_admission();
        let lifecycles = self
            .runtimes
            .get()
            .map_or_else(Vec::new, RtmpRuntimeSet::recorder_lifecycles);
        RtmpRetirement { lifecycles }
    }

    pub(crate) fn recorder_lifecycles(&self) -> Vec<RtmpRecorderLifecycle> {
        self.runtimes
            .get()
            .map_or_else(Vec::new, RtmpRuntimeSet::recorder_lifecycles)
    }

    pub(crate) fn start_prepared(&self) -> Result<RtmpRuntimeSet, GenerationError> {
        let prepared = self
            .prepared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or(GenerationError::RuntimePrepare)?;
        prepared
            .start(
                Arc::clone(&self.registry),
                Instant::now() + Duration::from_secs(5),
            )
            .map_err(map_runtime_set_error)
    }

    pub(crate) fn install(&self, runtimes: RtmpRuntimeSet) -> Result<(), GenerationError> {
        self.runtimes
            .set(runtimes)
            .map_err(|_| GenerationError::RuntimePrepare)
    }
}

fn map_runtime_set_error(error: RtmpRuntimeSetError) -> GenerationError {
    let timed_out = matches!(
        &error,
        RtmpRuntimeSetError::PreparationTimedOut { .. } | RtmpRuntimeSetError::StartTimedOut { .. }
    );
    drop(error);
    if timed_out {
        GenerationError::PreparationTimedOut
    } else {
        GenerationError::RuntimePrepare
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
