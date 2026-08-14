use std::fmt;

use crate::{
    ConnectionGuard, GenerationAdmission, GenerationReference, ListenerMetrics, MetricsError,
    RuntimeGeneration, RuntimeReferenceKind,
};

#[derive(Debug)]
pub enum AdmissionError {
    GenerationNotAccepting,
    Metrics(MetricsError),
}

impl fmt::Display for AdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::GenerationNotAccepting => formatter.write_str("generation is not accepting"),
            Self::Metrics(error) => error.fmt(formatter),
        }
    }
}

#[derive(Clone)]
pub struct ListenerRuntime {
    metrics: ListenerMetrics,
}

impl ListenerRuntime {
    #[must_use]
    pub const fn new(metrics: ListenerMetrics) -> Self {
        Self { metrics }
    }

    #[must_use]
    pub fn accepting(&self) -> bool {
        self.metrics.accepting()
    }

    /// Acquires generation ownership and listener/process connection capacity.
    ///
    /// # Errors
    ///
    /// Returns an error when the generation is not accepting or connection capacity is unavailable.
    pub fn admit(
        &self,
        generation: &std::sync::Arc<RuntimeGeneration>,
        kind: RuntimeReferenceKind,
    ) -> Result<TrafficLease, AdmissionError> {
        let generation_admission = generation
            .begin_admission()
            .ok_or(AdmissionError::GenerationNotAccepting)?;
        let generation_reference = generation
            .begin_reference(kind)
            .ok_or(AdmissionError::GenerationNotAccepting)?;
        let mut connection = self
            .metrics
            .begin_connection()
            .map_err(AdmissionError::Metrics)?;
        connection.suppress_access_record();
        Ok(TrafficLease {
            connection,
            _generation_reference: Some(generation_reference),
            _generation_admission: Some(generation_admission),
        })
    }

    /// Acquires listener/process capacity while converting an existing accept-gate claim.
    ///
    /// # Errors
    ///
    /// Returns an error when connection capacity is unavailable.
    pub fn admit_owned(
        &self,
        generation: &std::sync::Arc<RuntimeGeneration>,
        kind: RuntimeReferenceKind,
    ) -> Result<TrafficLease, AdmissionError> {
        let generation_reference = generation.begin_owned_reference(kind);
        let mut connection = self
            .metrics
            .begin_connection()
            .map_err(AdmissionError::Metrics)?;
        connection.suppress_access_record();
        Ok(TrafficLease {
            connection,
            _generation_reference: Some(generation_reference),
            _generation_admission: None,
        })
    }

    /// Acquires generation ownership and listener/process capacity for a control connection.
    ///
    /// Control admission intentionally bypasses process administrative drain so local management
    /// remains available to inspect and reverse a drain. Listener drain and capacity still apply.
    ///
    /// # Errors
    ///
    /// Returns an error when the generation is not accepting or connection capacity is unavailable.
    pub fn admit_control(
        &self,
        generation: &std::sync::Arc<RuntimeGeneration>,
        kind: RuntimeReferenceKind,
    ) -> Result<TrafficLease, AdmissionError> {
        let generation_admission = generation
            .begin_admission()
            .ok_or(AdmissionError::GenerationNotAccepting)?;
        let generation_reference = generation
            .begin_reference(kind)
            .ok_or(AdmissionError::GenerationNotAccepting)?;
        let mut connection = self
            .metrics
            .begin_control_connection()
            .map_err(AdmissionError::Metrics)?;
        connection.suppress_access_record();
        Ok(TrafficLease {
            connection,
            _generation_reference: Some(generation_reference),
            _generation_admission: Some(generation_admission),
        })
    }

    /// Acquires control-plane listener/process capacity from an existing accept-gate claim.
    ///
    /// # Errors
    ///
    /// Returns an error when listener or process capacity is unavailable.
    pub fn admit_owned_control(
        &self,
        generation: &std::sync::Arc<RuntimeGeneration>,
        kind: RuntimeReferenceKind,
    ) -> Result<TrafficLease, AdmissionError> {
        let generation_reference = generation.begin_owned_reference(kind);
        let mut connection = self
            .metrics
            .begin_control_connection()
            .map_err(AdmissionError::Metrics)?;
        connection.suppress_access_record();
        Ok(TrafficLease {
            connection,
            _generation_reference: Some(generation_reference),
            _generation_admission: None,
        })
    }

    pub(crate) fn admit_runtime_connection(
        &self,
        generation: &std::sync::Arc<RuntimeGeneration>,
        kind: RuntimeReferenceKind,
    ) -> Result<TrafficLease, AdmissionError> {
        let generation_reference = generation
            .begin_reference(kind)
            .ok_or(AdmissionError::GenerationNotAccepting)?;
        let mut connection = self
            .metrics
            .begin_connection()
            .map_err(AdmissionError::Metrics)?;
        connection.suppress_access_record();
        Ok(TrafficLease {
            connection,
            _generation_reference: Some(generation_reference),
            _generation_admission: None,
        })
    }
}

pub struct TrafficLease {
    connection: ConnectionGuard,
    _generation_reference: Option<GenerationReference>,
    _generation_admission: Option<GenerationAdmission>,
}

impl TrafficLease {
    pub(crate) fn record_bytes_received(&self, bytes: u64) -> Result<(), MetricsError> {
        self.connection.record_bytes_received(bytes)
    }

    pub(crate) fn record_bytes_sent(&self, bytes: u64) -> Result<(), MetricsError> {
        self.connection.record_bytes_sent(bytes)
    }

    pub(crate) fn record_proxy_protocol(
        &self,
        result: crate::ProxyProtocolResult,
    ) -> Result<(), MetricsError> {
        self.connection.record_proxy_protocol(result)
    }
}

#[cfg(test)]
mod tests {
    use oxiroute_config::ConfigDraft;

    use super::*;

    fn generation() -> std::sync::Arc<crate::RuntimeGeneration> {
        let config = ConfigDraft {
            version: 1,
            max_connections: None,
            management: None,
            stats: None,
            certificates: Vec::new(),
            tls_profiles: Vec::new(),
            listeners: Vec::new(),
            cache_stores: Vec::new(),
            upstream_pools: Vec::new(),
            http_services: Vec::new(),
            forward_proxy_services: Vec::new(),
            rtmp_services: Vec::new(),
            l4_services: Vec::new(),
        };
        let manager = crate::GenerationManager::new();
        let candidate = manager
            .prepare(crate::config_coordinator::ResolvedConfigDocument {
                authored_revision: crate::config_coordinator::AuthoredRevision::from_bytes(
                    b"listener-runtime-test",
                ),
                effective_revision: crate::config_coordinator::EffectiveRevision::from_bytes(
                    b"listener-runtime-test",
                ),
                validated_config: config.validate().expect("valid generation config"),
                format: oxiroute_config_source::ConfigFormat::Lua,
                compositional: false,
                dependencies: Vec::new(),
                config_preview: String::new(),
                diagnostics: Vec::new(),
            })
            .expect("generation candidate");
        manager.activate(&candidate).expect("active generation")
    }

    #[test]
    fn listener_admission_rolls_back_on_metrics_rejection_and_drains_after_release() {
        let generation = generation();
        let metrics = crate::RuntimeMetrics::with_max_connections(Some(1));
        let listener = metrics
            .register_listener("edge", "forward_http1", "127.0.0.1:8080", Some(1))
            .expect("listener");
        let runtime = ListenerRuntime::new(listener);

        metrics
            .set_listener_administrative_state("edge", crate::AdministrativeState::Drain)
            .expect("listener drain");
        assert!(
            runtime
                .admit(&generation, crate::RuntimeReferenceKind::ForwardHttp1)
                .is_err()
        );
        assert_eq!(
            generation.active_references(crate::RuntimeReferenceKind::ForwardHttp1),
            0
        );
        metrics
            .set_listener_administrative_state("edge", crate::AdministrativeState::Ready)
            .expect("listener ready");

        let held = runtime
            .admit_owned(&generation, crate::RuntimeReferenceKind::ForwardHttp1)
            .expect("held listener admission");

        assert!(
            runtime
                .admit(&generation, crate::RuntimeReferenceKind::ForwardHttp1)
                .is_err()
        );
        assert_eq!(
            generation.active_references(crate::RuntimeReferenceKind::ForwardHttp1),
            1
        );
        generation.stop_accepting();
        assert!(!generation.drain(std::time::Duration::ZERO));

        drop(held);
        assert!(generation.drain(std::time::Duration::from_millis(100)));
        assert_eq!(
            metrics
                .snapshot()
                .expect("released snapshot")
                .traffic
                .active_connections,
            0
        );
    }

    #[test]
    fn control_admission_rolls_back_and_bypasses_only_process_drain() {
        let generation = generation();
        let metrics = crate::RuntimeMetrics::with_max_connections(Some(1));
        let listener = metrics
            .register_listener("management", "http", "127.0.0.1:8080", Some(1))
            .expect("listener");
        let runtime = ListenerRuntime::new(listener);

        metrics
            .set_listener_administrative_state("management", crate::AdministrativeState::Drain)
            .expect("listener drain");
        assert!(
            runtime
                .admit_control(&generation, crate::RuntimeReferenceKind::Http1)
                .is_err()
        );
        assert_eq!(
            generation.active_references(crate::RuntimeReferenceKind::Http1),
            0
        );

        metrics
            .set_listener_administrative_state("management", crate::AdministrativeState::Ready)
            .expect("listener ready");
        metrics.set_process_administrative_state(crate::AdministrativeState::Drain);
        let lease = runtime
            .admit_control(&generation, crate::RuntimeReferenceKind::Http1)
            .expect("control admission during process drain");
        generation.stop_accepting();
        assert!(!generation.drain(std::time::Duration::ZERO));

        drop(lease);
        assert!(generation.drain(std::time::Duration::from_millis(100)));
        let snapshot = metrics.snapshot().expect("released snapshot");
        assert_eq!(snapshot.process.active_connections, 0);
        assert_eq!(snapshot.listeners[0].active_connections, 0);
        assert!(snapshot.access_records.is_empty());
    }
}
