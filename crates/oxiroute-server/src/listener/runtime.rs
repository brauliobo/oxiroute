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
mod tests;
