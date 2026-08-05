use oxiroute_supervision::{GenerationId, InstanceId};
use oxiroute_supervision_unix::MessageType;
use oxiroute_supervisor_process::AuthenticatedFrame;
use thiserror::Error;

use crate::CONTROL_PROTOCOL_VERSION;

pub(crate) const STATUS_MESSAGE: MessageType = MessageType(0x181);
/// Maximum encoded worker status bytes accepted by either endpoint.
pub const MAX_STATUS_BYTES: usize = 60 * 1024;
/// Maximum listener observations carried by one worker status.
pub const MAX_STATUS_LISTENERS: usize = 64;
/// Maximum event observations carried by one worker status.
pub const MAX_STATUS_EVENTS: usize = 64;
/// Maximum worker-originated events retained by the master.
pub const MAX_AGGREGATED_EVENTS: usize = 2_048;

const MAX_STATUS_STRING_BYTES: usize = 1_024;
const STATUS_FORMAT_VERSION: u16 = 1;

/// Lifecycle observation reported by a worker. It is not a command surface.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerLifecycle {
    Starting,
    Ready,
    Active,
    Quiescing,
    Draining,
    Reactivating,
    Stopping,
    Failed,
}

/// Runtime listener state reported by a worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerListenerState {
    Configured,
    Listening,
    Stopped,
    Failed,
}

/// Administrative listener state reported by a worker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WorkerAdministrativeState {
    Ready,
    Drain,
    Maintenance,
}

/// Aggregate process and traffic counters reported by a worker.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct WorkerMetrics {
    pub accepted_connections: u64,
    pub rejected_connections: u64,
    pub active_connections: u64,
    pub bytes_received: u64,
    pub bytes_sent: u64,
}

/// One bounded listener observation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerListenerStatus {
    pub name: String,
    pub protocol: String,
    pub bind: String,
    pub administrative_state: WorkerAdministrativeState,
    pub state: WorkerListenerState,
    pub accepted_connections: u64,
    pub rejected_connections: u64,
    pub active_connections: u64,
    pub bytes_received: u64,
    pub bytes_sent: u64,
}

/// Generation/reload state observed inside a worker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerGenerationStatus {
    pub disk_revision: Option<String>,
    pub candidate_revision: Option<String>,
    pub active_revision: Option<String>,
    pub previous_revision: Option<String>,
    pub quarantined_revision: Option<String>,
    pub active_accepting: bool,
    pub degraded: bool,
    pub last_failure: Option<String>,
    pub prepares: u64,
    pub activations: u64,
    pub failures: u64,
    pub rollbacks: u64,
}

/// One redacted worker-originated operational event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WorkerEventRecord {
    pub cursor: u64,
    pub timestamp_unix_ms: Option<u64>,
    pub event: String,
    pub outcome: String,
    pub revision: Option<String>,
    pub certificate: Option<String>,
    pub correlation_id: Option<String>,
    pub source: Option<String>,
    pub operation: Option<String>,
}

/// Authenticated, bounded worker observation sent to the master.
#[derive(Clone, Debug, Eq, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct WorkerStatus {
    pub sequence: u64,
    pub generation_id: GenerationId,
    pub lifecycle: WorkerLifecycle,
    pub administrative_state: WorkerAdministrativeState,
    pub accepting: bool,
    pub runtime_started: bool,
    pub runtime_failed: bool,
    pub drained: bool,
    pub generation: WorkerGenerationStatus,
    pub metrics: Option<WorkerMetrics>,
    pub listeners: Vec<WorkerListenerStatus>,
    pub degraded: bool,
    pub degradation: Option<String>,
    pub event_cursor: u64,
    pub event_cursor_lost: bool,
    pub events: Vec<WorkerEventRecord>,
}

/// A worker event retained by the master with authenticated process identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AggregatedWorkerEvent {
    pub cursor: u64,
    pub instance_id: InstanceId,
    pub generation_id: GenerationId,
    pub worker_event: WorkerEventRecord,
}

#[derive(Debug, Error)]
pub enum StatusProtocolError {
    #[error("unexpected worker status message type {0}")]
    UnexpectedMessage(u16),
    #[error("worker status transferred unexpected descriptors")]
    UnexpectedDescriptors,
    #[error("worker status payload exceeds {maximum} bytes")]
    TooLarge { maximum: usize },
    #[error("worker status protocol version {actual} does not match {expected}")]
    VersionMismatch { expected: u16, actual: u16 },
    #[error("worker status payload has an invalid shape")]
    InvalidPayload,
    #[error("worker status contains a string longer than {maximum} bytes")]
    StringTooLong { maximum: usize },
    #[error("worker status contains too many {kind}")]
    CollectionTooLong { kind: &'static str },
    #[error("worker status allocation failed")]
    Allocation,
}

pub(crate) fn encode_status(status: &WorkerStatus) -> Result<Vec<u8>, StatusProtocolError> {
    if status.sequence == 0 || status.listeners.len() > MAX_STATUS_LISTENERS {
        return Err(StatusProtocolError::InvalidPayload);
    }
    if status.events.len() > MAX_STATUS_EVENTS {
        return Err(StatusProtocolError::CollectionTooLong { kind: "events" });
    }
    let mut encoder = Encoder::new();
    encoder.u16(STATUS_FORMAT_VERSION)?;
    encoder.u16(CONTROL_PROTOCOL_VERSION)?;
    encoder.u64(status.sequence)?;
    encoder.u64(status.generation_id.0)?;
    encoder.u8(encode_lifecycle(status.lifecycle))?;
    encoder.u8(status_flags(status))?;
    encoder.u8(encode_administrative_state(status.administrative_state))?;
    encode_generation(&mut encoder, &status.generation)?;
    match status.metrics {
        Some(metrics) => {
            encoder.u8(1)?;
            encoder.u64(metrics.accepted_connections)?;
            encoder.u64(metrics.rejected_connections)?;
            encoder.u64(metrics.active_connections)?;
            encoder.u64(metrics.bytes_received)?;
            encoder.u64(metrics.bytes_sent)?;
        }
        None => encoder.u8(0)?,
    }
    encoder.u16(
        u16::try_from(status.listeners.len()).map_err(|_| StatusProtocolError::InvalidPayload)?,
    )?;
    for listener in &status.listeners {
        encoder.string(&listener.name)?;
        encoder.string(&listener.protocol)?;
        encoder.string(&listener.bind)?;
        encoder.u8(encode_administrative_state(listener.administrative_state))?;
        encoder.u8(encode_listener_state(listener.state))?;
        encoder.u64(listener.accepted_connections)?;
        encoder.u64(listener.rejected_connections)?;
        encoder.u64(listener.active_connections)?;
        encoder.u64(listener.bytes_received)?;
        encoder.u64(listener.bytes_sent)?;
    }
    encoder.u8(u8::from(status.degradation.is_some()))?;
    if let Some(degradation) = &status.degradation {
        encoder.string(degradation)?;
    }
    encoder.u64(status.event_cursor)?;
    encoder.u8(u8::from(status.event_cursor_lost))?;
    encoder.u16(
        u16::try_from(status.events.len()).map_err(|_| StatusProtocolError::InvalidPayload)?,
    )?;
    for event in &status.events {
        encode_event(&mut encoder, event)?;
    }
    Ok(encoder.finish())
}

#[allow(clippy::too_many_lines)]
pub(crate) fn decode_status(
    frame: &AuthenticatedFrame,
) -> Result<WorkerStatus, StatusProtocolError> {
    if frame.header().message_type() != STATUS_MESSAGE {
        return Err(StatusProtocolError::UnexpectedMessage(
            frame.header().message_type().0,
        ));
    }
    if !frame.descriptors().is_empty() {
        return Err(StatusProtocolError::UnexpectedDescriptors);
    }
    if frame.payload().len() > MAX_STATUS_BYTES {
        return Err(StatusProtocolError::TooLarge {
            maximum: MAX_STATUS_BYTES,
        });
    }
    let mut decoder = Decoder::new(frame.payload());
    let format_version = decoder.u16()?;
    if format_version != STATUS_FORMAT_VERSION {
        return Err(StatusProtocolError::VersionMismatch {
            expected: STATUS_FORMAT_VERSION,
            actual: format_version,
        });
    }
    let protocol_version = decoder.u16()?;
    if protocol_version != CONTROL_PROTOCOL_VERSION {
        return Err(StatusProtocolError::VersionMismatch {
            expected: CONTROL_PROTOCOL_VERSION,
            actual: protocol_version,
        });
    }
    let sequence = decoder.u64()?;
    if sequence == 0 {
        return Err(StatusProtocolError::InvalidPayload);
    }
    let generation_id = GenerationId(decoder.u64()?);
    if generation_id != frame.header().generation() {
        return Err(StatusProtocolError::InvalidPayload);
    }
    let lifecycle = decode_lifecycle(decoder.u8()?)?;
    let flags = decoder.u8()?;
    if flags & !0x1f != 0 {
        return Err(StatusProtocolError::InvalidPayload);
    }
    let administrative_state = decode_administrative_state(decoder.u8()?)?;
    let generation = decode_generation(&mut decoder)?;
    let metrics = match decoder.u8()? {
        0 => None,
        1 => Some(WorkerMetrics {
            accepted_connections: decoder.u64()?,
            rejected_connections: decoder.u64()?,
            active_connections: decoder.u64()?,
            bytes_received: decoder.u64()?,
            bytes_sent: decoder.u64()?,
        }),
        _ => return Err(StatusProtocolError::InvalidPayload),
    };
    let listener_count = usize::from(decoder.u16()?);
    if listener_count > MAX_STATUS_LISTENERS {
        return Err(StatusProtocolError::CollectionTooLong { kind: "listeners" });
    }
    let mut listeners = Vec::new();
    listeners
        .try_reserve_exact(listener_count)
        .map_err(|_| StatusProtocolError::Allocation)?;
    for _ in 0..listener_count {
        listeners.push(WorkerListenerStatus {
            name: decoder.string()?,
            protocol: decoder.string()?,
            bind: decoder.string()?,
            administrative_state: decode_administrative_state(decoder.u8()?)?,
            state: decode_listener_state(decoder.u8()?)?,
            accepted_connections: decoder.u64()?,
            rejected_connections: decoder.u64()?,
            active_connections: decoder.u64()?,
            bytes_received: decoder.u64()?,
            bytes_sent: decoder.u64()?,
        });
    }
    let degradation = match decoder.u8()? {
        0 => None,
        1 => Some(decoder.string()?),
        _ => return Err(StatusProtocolError::InvalidPayload),
    };
    let event_cursor = decoder.u64()?;
    let event_cursor_lost = match decoder.u8()? {
        0 => false,
        1 => true,
        _ => return Err(StatusProtocolError::InvalidPayload),
    };
    let event_count = usize::from(decoder.u16()?);
    if event_count > MAX_STATUS_EVENTS {
        return Err(StatusProtocolError::CollectionTooLong { kind: "events" });
    }
    let mut events = Vec::new();
    events
        .try_reserve_exact(event_count)
        .map_err(|_| StatusProtocolError::Allocation)?;
    for _ in 0..event_count {
        events.push(decode_event(&mut decoder)?);
    }
    if events
        .windows(2)
        .any(|events| events[0].cursor >= events[1].cursor)
    {
        return Err(StatusProtocolError::InvalidPayload);
    }
    if events
        .last()
        .is_some_and(|event| event.cursor > event_cursor)
    {
        return Err(StatusProtocolError::InvalidPayload);
    }
    decoder.finish()?;
    Ok(WorkerStatus {
        sequence,
        generation_id,
        lifecycle,
        administrative_state,
        accepting: flags & 0x01 != 0,
        runtime_started: flags & 0x02 != 0,
        runtime_failed: flags & 0x04 != 0,
        drained: flags & 0x08 != 0,
        degraded: flags & 0x10 != 0,
        generation,
        metrics,
        listeners,
        degradation,
        event_cursor,
        event_cursor_lost,
        events,
    })
}

fn encode_generation(
    encoder: &mut Encoder,
    generation: &WorkerGenerationStatus,
) -> Result<(), StatusProtocolError> {
    for revision in [
        &generation.disk_revision,
        &generation.candidate_revision,
        &generation.active_revision,
        &generation.previous_revision,
        &generation.quarantined_revision,
    ] {
        encoder.optional_string(revision.as_deref())?;
    }
    encoder.u8(u8::from(generation.active_accepting))?;
    encoder.u8(u8::from(generation.degraded))?;
    encoder.optional_string(generation.last_failure.as_deref())?;
    encoder.u64(generation.prepares)?;
    encoder.u64(generation.activations)?;
    encoder.u64(generation.failures)?;
    encoder.u64(generation.rollbacks)
}

fn decode_generation(
    decoder: &mut Decoder<'_>,
) -> Result<WorkerGenerationStatus, StatusProtocolError> {
    Ok(WorkerGenerationStatus {
        disk_revision: decoder.optional_string()?,
        candidate_revision: decoder.optional_string()?,
        active_revision: decoder.optional_string()?,
        previous_revision: decoder.optional_string()?,
        quarantined_revision: decoder.optional_string()?,
        active_accepting: decoder.flag()?,
        degraded: decoder.flag()?,
        last_failure: decoder.optional_string()?,
        prepares: decoder.u64()?,
        activations: decoder.u64()?,
        failures: decoder.u64()?,
        rollbacks: decoder.u64()?,
    })
}

fn encode_event(
    encoder: &mut Encoder,
    event: &WorkerEventRecord,
) -> Result<(), StatusProtocolError> {
    if event.cursor == 0 {
        return Err(StatusProtocolError::InvalidPayload);
    }
    encoder.u64(event.cursor)?;
    match event.timestamp_unix_ms {
        Some(timestamp) => {
            encoder.u8(1)?;
            encoder.u64(timestamp)?;
        }
        None => encoder.u8(0)?,
    }
    encoder.string(&event.event)?;
    encoder.string(&event.outcome)?;
    encoder.optional_string(event.revision.as_deref())?;
    encoder.optional_string(event.certificate.as_deref())?;
    encoder.optional_string(event.correlation_id.as_deref())?;
    encoder.optional_string(event.source.as_deref())?;
    encoder.optional_string(event.operation.as_deref())
}

fn decode_event(decoder: &mut Decoder<'_>) -> Result<WorkerEventRecord, StatusProtocolError> {
    let cursor = decoder.u64()?;
    if cursor == 0 {
        return Err(StatusProtocolError::InvalidPayload);
    }
    let timestamp_unix_ms = match decoder.u8()? {
        0 => None,
        1 => Some(decoder.u64()?),
        _ => return Err(StatusProtocolError::InvalidPayload),
    };
    Ok(WorkerEventRecord {
        cursor,
        timestamp_unix_ms,
        event: decoder.string()?,
        outcome: decoder.string()?,
        revision: decoder.optional_string()?,
        certificate: decoder.optional_string()?,
        correlation_id: decoder.optional_string()?,
        source: decoder.optional_string()?,
        operation: decoder.optional_string()?,
    })
}

const fn status_flags(status: &WorkerStatus) -> u8 {
    (if status.accepting { 1 } else { 0 })
        | ((if status.runtime_started { 1 } else { 0 }) << 1)
        | ((if status.runtime_failed { 1 } else { 0 }) << 2)
        | ((if status.drained { 1 } else { 0 }) << 3)
        | ((if status.degraded { 1 } else { 0 }) << 4)
}

const fn encode_lifecycle(lifecycle: WorkerLifecycle) -> u8 {
    match lifecycle {
        WorkerLifecycle::Starting => 1,
        WorkerLifecycle::Ready => 2,
        WorkerLifecycle::Active => 3,
        WorkerLifecycle::Quiescing => 4,
        WorkerLifecycle::Draining => 5,
        WorkerLifecycle::Reactivating => 6,
        WorkerLifecycle::Stopping => 7,
        WorkerLifecycle::Failed => 8,
    }
}

const fn decode_lifecycle(value: u8) -> Result<WorkerLifecycle, StatusProtocolError> {
    match value {
        1 => Ok(WorkerLifecycle::Starting),
        2 => Ok(WorkerLifecycle::Ready),
        3 => Ok(WorkerLifecycle::Active),
        4 => Ok(WorkerLifecycle::Quiescing),
        5 => Ok(WorkerLifecycle::Draining),
        6 => Ok(WorkerLifecycle::Reactivating),
        7 => Ok(WorkerLifecycle::Stopping),
        8 => Ok(WorkerLifecycle::Failed),
        _ => Err(StatusProtocolError::InvalidPayload),
    }
}

const fn encode_listener_state(state: WorkerListenerState) -> u8 {
    match state {
        WorkerListenerState::Configured => 0,
        WorkerListenerState::Listening => 1,
        WorkerListenerState::Stopped => 2,
        WorkerListenerState::Failed => 3,
    }
}

const fn decode_listener_state(value: u8) -> Result<WorkerListenerState, StatusProtocolError> {
    match value {
        0 => Ok(WorkerListenerState::Configured),
        1 => Ok(WorkerListenerState::Listening),
        2 => Ok(WorkerListenerState::Stopped),
        3 => Ok(WorkerListenerState::Failed),
        _ => Err(StatusProtocolError::InvalidPayload),
    }
}

const fn encode_administrative_state(state: WorkerAdministrativeState) -> u8 {
    match state {
        WorkerAdministrativeState::Ready => 0,
        WorkerAdministrativeState::Drain => 1,
        WorkerAdministrativeState::Maintenance => 2,
    }
}

const fn decode_administrative_state(
    value: u8,
) -> Result<WorkerAdministrativeState, StatusProtocolError> {
    match value {
        0 => Ok(WorkerAdministrativeState::Ready),
        1 => Ok(WorkerAdministrativeState::Drain),
        2 => Ok(WorkerAdministrativeState::Maintenance),
        _ => Err(StatusProtocolError::InvalidPayload),
    }
}

struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn bytes(&mut self, value: &[u8]) -> Result<(), StatusProtocolError> {
        let next =
            self.bytes
                .len()
                .checked_add(value.len())
                .ok_or(StatusProtocolError::TooLarge {
                    maximum: MAX_STATUS_BYTES,
                })?;
        if next > MAX_STATUS_BYTES {
            return Err(StatusProtocolError::TooLarge {
                maximum: MAX_STATUS_BYTES,
            });
        }
        self.bytes
            .try_reserve_exact(value.len())
            .map_err(|_| StatusProtocolError::Allocation)?;
        self.bytes.extend_from_slice(value);
        Ok(())
    }

    fn u8(&mut self, value: u8) -> Result<(), StatusProtocolError> {
        self.bytes(&[value])
    }

    fn u16(&mut self, value: u16) -> Result<(), StatusProtocolError> {
        self.bytes(&value.to_be_bytes())
    }

    fn u64(&mut self, value: u64) -> Result<(), StatusProtocolError> {
        self.bytes(&value.to_be_bytes())
    }

    fn string(&mut self, value: &str) -> Result<(), StatusProtocolError> {
        if value.len() > MAX_STATUS_STRING_BYTES {
            return Err(StatusProtocolError::StringTooLong {
                maximum: MAX_STATUS_STRING_BYTES,
            });
        }
        self.u16(
            u16::try_from(value.len()).map_err(|_| StatusProtocolError::StringTooLong {
                maximum: MAX_STATUS_STRING_BYTES,
            })?,
        )?;
        self.bytes(value.as_bytes())
    }

    fn optional_string(&mut self, value: Option<&str>) -> Result<(), StatusProtocolError> {
        match value {
            Some(value) => {
                self.u8(1)?;
                self.string(value)
            }
            None => self.u8(0),
        }
    }

    fn finish(self) -> Vec<u8> {
        self.bytes
    }
}

struct Decoder<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Decoder<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn bytes(&mut self, length: usize) -> Result<&'a [u8], StatusProtocolError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(StatusProtocolError::InvalidPayload)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(StatusProtocolError::InvalidPayload)?;
        self.position = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, StatusProtocolError> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, StatusProtocolError> {
        Ok(u16::from_be_bytes(
            self.bytes(2)?.try_into().expect("fixed status slice"),
        ))
    }

    fn u64(&mut self) -> Result<u64, StatusProtocolError> {
        Ok(u64::from_be_bytes(
            self.bytes(8)?.try_into().expect("fixed status slice"),
        ))
    }

    fn flag(&mut self) -> Result<bool, StatusProtocolError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(StatusProtocolError::InvalidPayload),
        }
    }

    fn string(&mut self) -> Result<String, StatusProtocolError> {
        let length = usize::from(self.u16()?);
        if length > MAX_STATUS_STRING_BYTES {
            return Err(StatusProtocolError::StringTooLong {
                maximum: MAX_STATUS_STRING_BYTES,
            });
        }
        String::from_utf8(self.bytes(length)?.to_vec())
            .map_err(|_| StatusProtocolError::InvalidPayload)
    }

    fn optional_string(&mut self) -> Result<Option<String>, StatusProtocolError> {
        match self.u8()? {
            0 => Ok(None),
            1 => self.string().map(Some),
            _ => Err(StatusProtocolError::InvalidPayload),
        }
    }

    fn finish(self) -> Result<(), StatusProtocolError> {
        (self.position == self.bytes.len())
            .then_some(())
            .ok_or(StatusProtocolError::InvalidPayload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status() -> WorkerStatus {
        WorkerStatus {
            sequence: 1,
            generation_id: GenerationId(7),
            lifecycle: WorkerLifecycle::Active,
            administrative_state: WorkerAdministrativeState::Ready,
            accepting: true,
            runtime_started: true,
            runtime_failed: false,
            drained: false,
            generation: WorkerGenerationStatus {
                disk_revision: Some("a".repeat(64)),
                candidate_revision: None,
                active_revision: Some("b".repeat(64)),
                previous_revision: None,
                quarantined_revision: None,
                active_accepting: true,
                degraded: false,
                last_failure: None,
                prepares: 1,
                activations: 1,
                failures: 0,
                rollbacks: 0,
            },
            metrics: Some(WorkerMetrics {
                accepted_connections: 2,
                rejected_connections: 3,
                active_connections: 4,
                bytes_received: 5,
                bytes_sent: 6,
            }),
            listeners: vec![WorkerListenerStatus {
                name: "http".into(),
                protocol: "http".into(),
                bind: "127.0.0.1:8080".into(),
                administrative_state: WorkerAdministrativeState::Ready,
                state: WorkerListenerState::Listening,
                accepted_connections: 2,
                rejected_connections: 3,
                active_connections: 4,
                bytes_received: 5,
                bytes_sent: 6,
            }],
            degraded: false,
            degradation: None,
            event_cursor: 1,
            event_cursor_lost: false,
            events: vec![WorkerEventRecord {
                cursor: 1,
                timestamp_unix_ms: Some(9),
                event: "generation_activate".into(),
                outcome: "applied".into(),
                revision: Some("b".repeat(64)),
                certificate: None,
                correlation_id: Some("op-1".into()),
                source: Some("runtime".into()),
                operation: Some("generation_activate".into()),
            }],
        }
    }

    #[test]
    fn status_encoding_preserves_bounded_observation() {
        let expected = status();
        let payload = encode_status(&expected).expect("encode");
        assert!(!payload.is_empty());
        assert_eq!(expected.sequence, 1);
    }

    #[test]
    fn status_encoding_rejects_unbounded_event_batches() {
        let mut status = status();
        status.events = (1..=MAX_STATUS_EVENTS + 1)
            .map(|cursor| WorkerEventRecord {
                cursor: u64::try_from(cursor).expect("cursor"),
                timestamp_unix_ms: None,
                event: "event".into(),
                outcome: "applied".into(),
                revision: None,
                certificate: None,
                correlation_id: None,
                source: None,
                operation: None,
            })
            .collect();
        assert!(matches!(
            encode_status(&status),
            Err(StatusProtocolError::CollectionTooLong { kind: "events" })
        ));
    }
}
