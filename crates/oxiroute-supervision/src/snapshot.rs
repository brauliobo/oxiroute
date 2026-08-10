use serde::{Deserialize, Deserializer, Serialize};
use thiserror::Error;

use crate::{GenerationId, Revision, ServiceId, validated::deserialize_validated};

/// A versioned service snapshot and its source identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SnapshotEnvelope<T> {
    format_version: u16,
    service_id: ServiceId,
    generation_id: GenerationId,
    revision: Revision,
    payload: T,
}

impl<T> SnapshotEnvelope<T> {
    /// Creates a snapshot envelope with a nonzero format version.
    ///
    /// # Errors
    ///
    /// Returns [`SnapshotError::ZeroVersion`] when `format_version` is zero.
    pub fn new(
        format_version: u16,
        service_id: ServiceId,
        generation_id: GenerationId,
        revision: Revision,
        payload: T,
    ) -> Result<Self, SnapshotError> {
        if format_version == 0 {
            return Err(SnapshotError::ZeroVersion);
        }
        Ok(Self {
            format_version,
            service_id,
            generation_id,
            revision,
            payload,
        })
    }

    /// Returns the snapshot format version.
    #[must_use]
    pub const fn format_version(&self) -> u16 {
        self.format_version
    }

    /// Returns the logical service identity.
    #[must_use]
    pub const fn service_id(&self) -> &ServiceId {
        &self.service_id
    }

    /// Returns the source generation.
    #[must_use]
    pub const fn generation_id(&self) -> GenerationId {
        self.generation_id
    }

    /// Returns the snapshot state revision.
    #[must_use]
    pub const fn revision(&self) -> Revision {
        self.revision
    }

    /// Returns the snapshot payload.
    #[must_use]
    pub const fn payload(&self) -> &T {
        &self.payload
    }

    /// Consumes the envelope and returns its payload.
    #[must_use]
    pub fn into_payload(self) -> T {
        self.payload
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for SnapshotEnvelope<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire<T> {
            format_version: u16,
            service_id: ServiceId,
            generation_id: GenerationId,
            revision: Revision,
            payload: T,
        }

        deserialize_validated(deserializer, |wire: Wire<T>| {
            Self::new(
                wire.format_version,
                wire.service_id,
                wire.generation_id,
                wire.revision,
                wire.payload,
            )
        })
    }
}

/// An invalid snapshot envelope.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SnapshotError {
    #[error("snapshot format version must be nonzero")]
    ZeroVersion,
}
