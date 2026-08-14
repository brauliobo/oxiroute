use std::fmt;

use crate::{
    ConnectionGuard, GenerationAdmission, GenerationReference, ListenerMetrics, MetricsError,
    RuntimeGeneration, RuntimeReferenceKind,
};

#[derive(Debug)]
pub(crate) enum AdmissionError {
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
pub(crate) struct ListenerRuntime {
    metrics: ListenerMetrics,
}

impl ListenerRuntime {
    pub(crate) const fn new(metrics: ListenerMetrics) -> Self {
        Self { metrics }
    }

    pub(crate) fn accepting(&self) -> bool {
        self.metrics.accepting()
    }

    pub(crate) fn admit(
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
            _connection: connection,
            _generation_reference: Some(generation_reference),
            _generation_admission: Some(generation_admission),
        })
    }

    pub(crate) fn admit_owned(
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
            _connection: connection,
            _generation_reference: Some(generation_reference),
            _generation_admission: None,
        })
    }

    pub(crate) fn admit_without_generation(&self) -> Result<TrafficLease, AdmissionError> {
        let mut connection = self
            .metrics
            .begin_connection()
            .map_err(AdmissionError::Metrics)?;
        connection.suppress_access_record();
        Ok(TrafficLease {
            _connection: connection,
            _generation_reference: None,
            _generation_admission: None,
        })
    }
}

pub(crate) struct TrafficLease {
    _connection: ConnectionGuard,
    _generation_reference: Option<GenerationReference>,
    _generation_admission: Option<GenerationAdmission>,
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
    fn listener_only_lease_releases_connection_capacity_on_drop() {
        let metrics = crate::RuntimeMetrics::with_max_connections(Some(1));
        let listener = metrics
            .register_listener("edge", "http", "127.0.0.1:8080", Some(1))
            .expect("listener");
        let runtime = ListenerRuntime::new(listener);
        let lease = runtime
            .admit_without_generation()
            .expect("listener admission");

        assert_eq!(
            metrics
                .snapshot()
                .expect("active snapshot")
                .traffic
                .active_connections,
            1
        );
        drop(lease);
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
    fn listener_lease_does_not_emit_a_synthetic_access_record() {
        let metrics = crate::RuntimeMetrics::with_max_connections(Some(1));
        let listener = metrics
            .register_listener("edge", "http", "127.0.0.1:8080", Some(1))
            .expect("listener metrics");
        let runtime = ListenerRuntime::new(listener);

        drop(
            runtime
                .admit_without_generation()
                .expect("listener admission"),
        );

        assert!(
            metrics
                .snapshot()
                .expect("snapshot")
                .access_records
                .is_empty()
        );
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
}
