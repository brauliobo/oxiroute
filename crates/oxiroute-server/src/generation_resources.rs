use std::sync::{Arc, Mutex};

use oxiroute_rtmp::{
    PreparedRtmpRuntimeSet, RtmpAutoPushStatus, RtmpControlHandle, RtmpRecorderShutdown,
    RtmpRegistry, RtmpServiceHandle,
};

use crate::{
    GenerationError, HealthSupervisor, ListenerReservations, PreparedTls, RoundRobinPool,
    RuntimePlan, ServiceSpec,
    monitoring::ListenerRegistrationTransaction,
    rtmp_generation_runtime::{RtmpGenerationRuntime, RtmpRetirement},
    service_plan::GenerationAcquisition,
};

pub(crate) struct GenerationResources {
    // Fields are declared in reverse ownership order so teardown releases runtime workers before
    // listeners, then releases the plans that own files, stores, pools, TLS, and other acquisitions.
    rtmp: RtmpGenerationRuntime,
    listeners: PreparedListenerResources,
    services: Vec<ServiceSpec>,
    health_supervisor: Option<HealthSupervisor>,
    pools: Vec<Arc<RoundRobinPool>>,
    tls: PreparedTls,
    plan: RuntimePlan,
    #[cfg(test)]
    drop_probe: Option<Arc<std::sync::atomic::AtomicUsize>>,
}

impl GenerationResources {
    pub(crate) fn commit(
        plan: RuntimePlan,
        mut acquired: GenerationAcquisition,
        reservations: ListenerReservations,
        listener_registration: ListenerRegistrationTransaction,
        rtmp: (Arc<RtmpRegistry>, PreparedRtmpRuntimeSet),
    ) -> Self {
        let (services, health_supervisor, pools, tls) = acquired.commit();
        let (registry, prepared) = rtmp;
        Self {
            rtmp: RtmpGenerationRuntime::new(registry, prepared),
            listeners: PreparedListenerResources {
                registration: Mutex::new(Some(listener_registration)),
                reservations,
            },
            services,
            health_supervisor,
            pools,
            tls,
            plan,
            #[cfg(test)]
            drop_probe: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn set_drop_probe(&mut self, probe: Arc<std::sync::atomic::AtomicUsize>) {
        self.drop_probe = Some(probe);
    }

    pub(crate) const fn plan(&self) -> &RuntimePlan {
        &self.plan
    }

    pub(crate) fn services(&self) -> &[ServiceSpec] {
        &self.services
    }

    pub(crate) fn health_supervisor(&self) -> Option<HealthSupervisor> {
        self.health_supervisor.clone()
    }

    pub(crate) fn pools(&self) -> &[Arc<RoundRobinPool>] {
        &self.pools
    }

    pub(crate) fn rtmp_vod_catalog(&self) -> Arc<oxiroute_rtmp::VodCatalog> {
        self.rtmp.vod_catalog()
    }

    pub(crate) fn rtmp_media_catalog(&self) -> Arc<oxiroute_rtmp::MediaCatalog> {
        self.rtmp.media_catalog()
    }

    pub(crate) const fn tls(&self) -> &PreparedTls {
        &self.tls
    }

    pub(crate) const fn reservations(&self) -> &ListenerReservations {
        &self.listeners.reservations
    }

    pub(crate) fn rtmp_control(&self) -> RtmpControlHandle {
        self.rtmp.control()
    }

    pub(crate) fn rtmp_service(&self, service: &str) -> Option<RtmpServiceHandle> {
        self.rtmp.service(service)
    }

    pub(crate) fn rtmp_auto_push_status(&self) -> RtmpAutoPushStatus {
        self.rtmp.auto_push_status()
    }

    pub(crate) fn close_runtime_admission(&self) {
        self.rtmp.close_admission();
    }

    pub(crate) fn initiate_recorder_shutdown(
        &self,
        deadline: std::time::Instant,
    ) -> Vec<RtmpRecorderShutdown> {
        self.rtmp.initiate_recorder_shutdown(deadline)
    }

    pub(crate) fn rtmp_retirement(&self) -> RtmpRetirement {
        self.rtmp.retirement()
    }

    pub(crate) fn rtmp_recorder_lifecycles(&self) -> Vec<oxiroute_rtmp::RtmpRecorderLifecycle> {
        self.rtmp.recorder_lifecycles()
    }

    pub(crate) fn start_rtmp(&self) -> Result<(), GenerationError> {
        // Preserve the existing rollback guard order: claim listener registration before consuming
        // prepared RTMP stores, start every runtime, then commit process listener state.
        let listener_registration = self.listeners.take_registration()?;
        #[cfg(test)]
        {
            let mut started = Vec::new();
            for service in &self.services {
                let crate::ServiceKind::Rtmp(service) = &service.kind else {
                    continue;
                };
                if started
                    .iter()
                    .any(|started| *started == service.service_id())
                {
                    continue;
                }
                if crate::service_plan::trace_staged_rtmp_start(service.service_id()) {
                    for service in started.into_iter().rev() {
                        crate::service_plan::trace_rtmp_rollback(service);
                    }
                    return Err(GenerationError::RuntimePrepare);
                }
                started.push(service.service_id());
            }
        }
        let runtimes = self.rtmp.start_prepared()?;
        listener_registration.commit()?;
        self.rtmp.install(runtimes)
    }
}

impl Drop for GenerationResources {
    fn drop(&mut self) {
        #[cfg(test)]
        if let Some(probe) = &self.drop_probe {
            probe.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

struct PreparedListenerResources {
    registration: Mutex<Option<ListenerRegistrationTransaction>>,
    reservations: ListenerReservations,
}

impl PreparedListenerResources {
    fn take_registration(&self) -> Result<ListenerRegistrationTransaction, GenerationError> {
        self.registration
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
            .ok_or(GenerationError::RuntimePrepare)
    }
}
