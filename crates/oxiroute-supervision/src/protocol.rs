use std::marker::PhantomData;

use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use thiserror::Error;

use crate::{CorrelationToken, GenerationId, InstanceId, Sequence, ServiceId};

/// Compile-time protocol payload association for a supervised service.
///
/// Associated payloads remain statically typed; implementations do not need to be object-safe.
pub trait ServiceProtocol {
    const VERSION: u16;
    type Request;
    type Response;
    type Error;
    type Event;
    type Snapshot;
}

/// Protocol metadata that failed validation.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ProtocolError {
    #[error("service protocol version must be nonzero")]
    ZeroVersion,
    #[error("expected service protocol version {expected}, but received {actual}")]
    VersionMismatch { expected: u16, actual: u16 },
}

/// Metadata shared by messages for protocol `P`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(bound(serialize = "T: Serialize"))]
pub struct MessageEnvelope<P: ServiceProtocol, T> {
    protocol_version: u16,
    service_id: ServiceId,
    instance_id: InstanceId,
    generation_id: GenerationId,
    payload: T,
    #[serde(skip)]
    protocol: PhantomData<fn() -> P>,
}

impl<P: ServiceProtocol, T> MessageEnvelope<P, T> {
    fn new(
        service_id: ServiceId,
        instance_id: InstanceId,
        generation_id: GenerationId,
        payload: T,
    ) -> Result<Self, ProtocolError> {
        if P::VERSION == 0 {
            return Err(ProtocolError::ZeroVersion);
        }
        Ok(Self {
            protocol_version: P::VERSION,
            service_id,
            instance_id,
            generation_id,
            payload,
            protocol: PhantomData,
        })
    }

    fn from_wire(
        protocol_version: u16,
        service_id: ServiceId,
        instance_id: InstanceId,
        generation_id: GenerationId,
        payload: T,
    ) -> Result<Self, ProtocolError> {
        if P::VERSION == 0 {
            return Err(ProtocolError::ZeroVersion);
        }
        if protocol_version != P::VERSION {
            return Err(ProtocolError::VersionMismatch {
                expected: P::VERSION,
                actual: protocol_version,
            });
        }
        Self::new(service_id, instance_id, generation_id, payload)
    }

    /// Returns the validated protocol version.
    #[must_use]
    pub const fn protocol_version(&self) -> u16 {
        self.protocol_version
    }

    /// Returns the logical service identity.
    #[must_use]
    pub const fn service_id(&self) -> &ServiceId {
        &self.service_id
    }

    /// Returns the runtime instance identity.
    #[must_use]
    pub const fn instance_id(&self) -> &InstanceId {
        &self.instance_id
    }

    /// Returns the service generation.
    #[must_use]
    pub const fn generation_id(&self) -> GenerationId {
        self.generation_id
    }

    /// Returns the typed message payload.
    #[must_use]
    pub const fn payload(&self) -> &T {
        &self.payload
    }
}

impl<'de, P, T> Deserialize<'de> for MessageEnvelope<P, T>
where
    P: ServiceProtocol,
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire<T> {
            protocol_version: u16,
            service_id: ServiceId,
            instance_id: InstanceId,
            generation_id: GenerationId,
            payload: T,
        }

        let wire = Wire::deserialize(deserializer)?;
        Self::from_wire(
            wire.protocol_version,
            wire.service_id,
            wire.instance_id,
            wire.generation_id,
            wire.payload,
        )
        .map_err(D::Error::custom)
    }
}

/// A correlated RPC request envelope for protocol `P`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(
    serialize = "P::Request: Serialize",
    deserialize = "P::Request: Deserialize<'de>"
))]
pub struct RequestEnvelope<P: ServiceProtocol> {
    correlation: CorrelationToken,
    #[serde(flatten)]
    message: MessageEnvelope<P, P::Request>,
}

impl<P: ServiceProtocol> RequestEnvelope<P> {
    /// Creates a request using `P::VERSION`.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::ZeroVersion`] when `P::VERSION` is zero.
    pub fn new(
        correlation: CorrelationToken,
        service_id: ServiceId,
        instance_id: InstanceId,
        generation_id: GenerationId,
        payload: P::Request,
    ) -> Result<Self, ProtocolError> {
        Ok(Self {
            correlation,
            message: MessageEnvelope::new(service_id, instance_id, generation_id, payload)?,
        })
    }

    /// Returns the unambiguous request correlation token.
    #[must_use]
    pub const fn correlation(&self) -> CorrelationToken {
        self.correlation
    }

    /// Returns the shared message metadata and payload.
    #[must_use]
    pub const fn message(&self) -> &MessageEnvelope<P, P::Request> {
        &self.message
    }
}

/// Success or service-defined failure returned by an RPC request.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "status", content = "value", rename_all = "snake_case")]
pub enum RpcOutcome<T, E> {
    Success(T),
    Error(E),
}

/// A correlated RPC response envelope for protocol `P`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(
    serialize = "P::Response: Serialize, P::Error: Serialize",
    deserialize = "P::Response: Deserialize<'de>, P::Error: Deserialize<'de>"
))]
pub struct ResponseEnvelope<P: ServiceProtocol> {
    correlation: CorrelationToken,
    #[serde(flatten)]
    message: MessageEnvelope<P, RpcOutcome<P::Response, P::Error>>,
}

impl<P: ServiceProtocol> ResponseEnvelope<P> {
    /// Creates a response using `P::VERSION` and the request's full correlation token.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::ZeroVersion`] when `P::VERSION` is zero.
    pub fn new(
        correlation: CorrelationToken,
        service_id: ServiceId,
        instance_id: InstanceId,
        generation_id: GenerationId,
        outcome: RpcOutcome<P::Response, P::Error>,
    ) -> Result<Self, ProtocolError> {
        Ok(Self {
            correlation,
            message: MessageEnvelope::new(service_id, instance_id, generation_id, outcome)?,
        })
    }

    /// Returns the unambiguous request correlation token.
    #[must_use]
    pub const fn correlation(&self) -> CorrelationToken {
        self.correlation
    }

    /// Returns the shared message metadata and outcome.
    #[must_use]
    pub const fn message(&self) -> &MessageEnvelope<P, RpcOutcome<P::Response, P::Error>> {
        &self.message
    }
}

/// An ordered service event envelope for protocol `P`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(bound(
    serialize = "P::Event: Serialize",
    deserialize = "P::Event: Deserialize<'de>"
))]
pub struct EventEnvelope<P: ServiceProtocol> {
    sequence: Sequence,
    #[serde(flatten)]
    message: MessageEnvelope<P, P::Event>,
}

impl<P: ServiceProtocol> EventEnvelope<P> {
    /// Creates an event using `P::VERSION`.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::ZeroVersion`] when `P::VERSION` is zero.
    pub fn new(
        sequence: Sequence,
        service_id: ServiceId,
        instance_id: InstanceId,
        generation_id: GenerationId,
        payload: P::Event,
    ) -> Result<Self, ProtocolError> {
        Ok(Self {
            sequence,
            message: MessageEnvelope::new(service_id, instance_id, generation_id, payload)?,
        })
    }

    /// Returns the event sequence.
    #[must_use]
    pub const fn sequence(&self) -> Sequence {
        self.sequence
    }

    /// Returns the shared message metadata and payload.
    #[must_use]
    pub const fn message(&self) -> &MessageEnvelope<P, P::Event> {
        &self.message
    }
}
