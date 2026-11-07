#[allow(clippy::wildcard_imports)]
use super::*;

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
    let mut encoder = Encoder::new(MAX_STATUS_BYTES);
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

trait StatusEncoder {
    fn string(&mut self, value: &str) -> Result<(), StatusProtocolError>;

    fn optional_string(&mut self, value: Option<&str>) -> Result<(), StatusProtocolError>;
}

impl StatusEncoder for BoundedWireWriter<StatusWire> {
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
}

trait StatusDecoder {
    fn flag(&mut self) -> Result<bool, StatusProtocolError>;

    fn string(&mut self) -> Result<String, StatusProtocolError>;

    fn optional_string(&mut self) -> Result<Option<String>, StatusProtocolError>;
}

impl StatusDecoder for BoundedWireReader<'_, StatusWire> {
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
}

struct StatusWire;

impl BoundedWireProtocol for StatusWire {
    type Error = StatusProtocolError;

    fn invalid() -> Self::Error {
        StatusProtocolError::InvalidPayload
    }

    fn too_large(_actual: usize, maximum: usize) -> Self::Error {
        StatusProtocolError::TooLarge { maximum }
    }

    fn allocation() -> Self::Error {
        StatusProtocolError::Allocation
    }
}

type Encoder = BoundedWireWriter<StatusWire>;
type Decoder<'a> = BoundedWireReader<'a, StatusWire>;
