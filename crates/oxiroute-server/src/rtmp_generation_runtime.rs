use std::{
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use oxiroute_rtmp::{
    PreparedRtmpRuntimeSet, RtmpAutoPushStatus, RtmpControlHandle, RtmpRuntimeSet,
    RtmpRuntimeSetError, RtmpServiceHandle, RtmpShutdown,
};

use crate::GenerationError;

pub(crate) struct RtmpGenerationRuntime {
    prepared: Mutex<Option<PreparedRtmpRuntimeSet>>,
    runtimes: OnceLock<RtmpRuntimeSet>,
}

#[derive(Clone)]
pub(crate) struct RtmpRetirement {
    shutdown: RtmpShutdown,
}

#[derive(Default)]
pub(crate) struct RtmpRetirementRegistry {
    retirements: Vec<RetiredRtmpLifecycle>,
}

struct RetiredRtmpLifecycle {
    shutdown: RtmpShutdown,
}

pub(crate) struct RtmpRetirementWork {
    shutdown: RtmpShutdown,
}

impl RtmpGenerationRuntime {
    pub(crate) fn new(prepared: PreparedRtmpRuntimeSet) -> Self {
        Self {
            prepared: Mutex::new(Some(prepared)),
            runtimes: OnceLock::new(),
        }
    }

    pub(crate) fn control(&self) -> RtmpControlHandle {
        self.runtimes
            .get()
            .expect("started RTMP runtime set")
            .control()
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

    pub(crate) fn initiate_shutdown(&self, deadline: Instant) -> RtmpShutdown {
        let shutdown = self.shutdown();
        self.close_admission();
        shutdown.initiate(deadline);
        shutdown
    }

    pub(crate) fn retirement(&self) -> RtmpRetirement {
        self.close_admission();
        RtmpRetirement {
            shutdown: self.shutdown(),
        }
    }

    pub(crate) fn shutdown(&self) -> RtmpShutdown {
        self.runtimes
            .get()
            .map_or_else(RtmpShutdown::default, RtmpRuntimeSet::shutdown_handle)
    }

    pub(crate) fn start_prepared(&self) -> Result<RtmpRuntimeSet, GenerationError> {
        let prepared = self
            .prepared
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or(GenerationError::RuntimePrepare)?;
        prepared
            .start(Instant::now() + Duration::from_secs(5))
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
        if !retirement.shutdown.is_complete()
            && !self
                .retirements
                .iter()
                .any(|existing| existing.shutdown.is_same_lifecycle(&retirement.shutdown))
        {
            self.retirements.push(RetiredRtmpLifecycle {
                shutdown: retirement.shutdown,
            });
        }
    }

    pub(crate) fn take_shutdown_work(&mut self) -> Vec<RtmpRetirementWork> {
        self.retirements
            .iter()
            .map(|retirement| RtmpRetirementWork {
                shutdown: retirement.shutdown.clone(),
            })
            .collect()
    }

    pub(crate) fn prune_completed(&mut self) {
        self.retirements
            .retain(|retirement| !retirement.shutdown.is_complete());
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.retirements.len()
    }
}

impl RtmpRetirementWork {
    pub(crate) fn initiate(self, deadline: Instant) -> RtmpShutdown {
        self.shutdown.initiate(deadline);
        self.shutdown
    }
}
