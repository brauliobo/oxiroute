use std::{fmt, net::SocketAddr, sync::Arc, time::Instant};

use crate::{
    ConnectionGuard, GenerationAdmission, GenerationReference, ListenerMetrics, MetricsError,
    RuntimeGeneration, RuntimeReferenceKind,
    clients::{Concurrent, ConnStats, Connection, Protocol},
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
            tracked: None,
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
            tracked: None,
        })
    }

    /// Acquires generation ownership and listener/process capacity for a control connection.
    ///
    /// Control admission intentionally bypasses process administrative drain and the operator drain of
    /// the generation's traffic, so local management remains available to inspect and reverse a drain.
    /// Listener drain and capacity still apply.
    ///
    /// # Errors
    ///
    /// Returns an error when the generation is replaced or connection capacity is unavailable.
    pub fn admit_control(
        &self,
        generation: &std::sync::Arc<RuntimeGeneration>,
    ) -> Result<TrafficLease, AdmissionError> {
        let generation_admission = generation
            .begin_control_admission()
            .ok_or(AdmissionError::GenerationNotAccepting)?;
        let generation_reference = generation
            .begin_control_reference()
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
            tracked: None,
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
    ) -> Result<TrafficLease, AdmissionError> {
        let generation_reference = generation.begin_owned_control_reference();
        let mut connection = self
            .metrics
            .begin_control_connection()
            .map_err(AdmissionError::Metrics)?;
        connection.suppress_access_record();
        Ok(TrafficLease {
            connection,
            _generation_reference: Some(generation_reference),
            _generation_admission: None,
            tracked: None,
        })
    }

    pub(crate) fn admit_tracked_connection(
        &self,
        generation: &std::sync::Arc<RuntimeGeneration>,
        kind: RuntimeReferenceKind,
        (peer, protocol): (SocketAddr, Protocol),
    ) -> Result<TrafficLease, AdmissionError> {
        let mut lease = self.admit_runtime_connection(generation, kind)?;
        lease.tracked = self.metrics.track(peer, protocol);
        Ok(lease)
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
            tracked: None,
        })
    }
}

pub struct TrafficLease {
    tracked: Option<Connection>,
    connection: ConnectionGuard,
    _generation_reference: Option<GenerationReference>,
    _generation_admission: Option<GenerationAdmission>,
}

impl TrafficLease {
    pub(crate) fn stats(&self) -> Option<&Arc<ConnStats>> {
        self.tracked.as_ref().map(Connection::stats)
    }

    pub(crate) fn record_completed(&self, failed: bool) {
        if let Some(stats) = self.stats() {
            stats.completed::<Concurrent>(failed, Instant::now());
        }
    }

    pub(crate) fn record_bytes_received(&self, bytes: u64) -> Result<(), MetricsError> {
        if let Some(stats) = self.stats() {
            stats.received::<Concurrent>(bytes, Instant::now());
        }
        self.connection.record_bytes_received(bytes)
    }

    pub(crate) fn record_bytes_sent(&self, bytes: u64) -> Result<(), MetricsError> {
        if let Some(stats) = self.stats() {
            stats.sent::<Concurrent>(bytes, Instant::now());
        }
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
mod tests;
