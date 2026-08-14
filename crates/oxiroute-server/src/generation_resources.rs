use std::sync::{Arc, Mutex};

use oxiroute_rtmp::{RtmpAutoPushStatus, RtmpRecorderShutdown, RtmpRegistry, RtmpServiceRuntime};

use crate::{
    GenerationError, HealthSupervisor, ListenerReservations, PreparedTls, RoundRobinPool,
    RuntimePlan, ServiceSpec,
    monitoring::ListenerRegistrationTransaction,
    rtmp_generation_runtime::{
        PreparedRtmpGenerationRuntime, RtmpGenerationRuntime, RtmpRetirement,
    },
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
        rtmp: PreparedRtmpGenerationRuntime,
    ) -> Self {
        let (services, health_supervisor, pools, tls) = acquired.commit();
        Self {
            rtmp: rtmp.commit(),
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

    pub(crate) const fn rtmp_vod_catalog(&self) -> &Arc<oxiroute_rtmp::VodCatalog> {
        self.rtmp.vod_catalog()
    }

    pub(crate) const fn rtmp_media_catalog(&self) -> &Arc<oxiroute_rtmp::MediaCatalog> {
        self.rtmp.media_catalog()
    }

    pub(crate) const fn tls(&self) -> &PreparedTls {
        &self.tls
    }

    pub(crate) const fn reservations(&self) -> &ListenerReservations {
        &self.listeners.reservations
    }

    pub(crate) const fn registry(&self) -> &Arc<RtmpRegistry> {
        self.rtmp.registry()
    }

    pub(crate) fn rtmp_runtime(&self, service: &str) -> Option<&RtmpServiceRuntime> {
        self.rtmp.runtime(service)
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

    pub(crate) fn start_rtmp(&self) -> Result<(), GenerationError> {
        // Preserve the existing rollback guard order: claim listener registration before consuming
        // prepared RTMP stores, start every runtime, then commit process listener state.
        let listener_registration = self.listeners.take_registration()?;
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
