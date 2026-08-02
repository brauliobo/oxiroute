use serde::{Deserialize, Deserializer, Serialize, de::Error as _};
use thiserror::Error;

use crate::{BoundError, BoundedVec, Sequence};

/// Stable numeric identifier for a metric descriptor.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(transparent)]
pub struct MetricId(pub u32);

/// The aggregation semantics of a metric.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetricKind {
    Counter,
    Gauge,
}

/// Static metadata for one metric ID.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MetricDescriptor {
    pub id: MetricId,
    pub name: &'static str,
    pub unit: &'static str,
    pub description: &'static str,
    pub kind: MetricKind,
}

/// A validated borrowed registry of static metric descriptors.
#[derive(Clone, Copy, Debug)]
pub struct MetricRegistry {
    descriptors: &'static [MetricDescriptor],
}

impl MetricRegistry {
    /// Validates a static descriptor table without allocating.
    ///
    /// # Errors
    ///
    /// Returns an error for empty names or duplicate numeric IDs or names.
    pub fn new(descriptors: &'static [MetricDescriptor]) -> Result<Self, MetricRegistryError> {
        for (index, descriptor) in descriptors.iter().enumerate() {
            if descriptor.name.is_empty() {
                return Err(MetricRegistryError::EmptyName { id: descriptor.id });
            }
            if descriptors[..index]
                .iter()
                .any(|existing| existing.id == descriptor.id)
            {
                return Err(MetricRegistryError::DuplicateId { id: descriptor.id });
            }
            if descriptors[..index]
                .iter()
                .any(|existing| existing.name == descriptor.name)
            {
                return Err(MetricRegistryError::DuplicateName {
                    name: descriptor.name,
                });
            }
        }
        Ok(Self { descriptors })
    }

    /// Returns the static descriptors in declaration order.
    #[must_use]
    pub const fn descriptors(self) -> &'static [MetricDescriptor] {
        self.descriptors
    }

    /// Resolves one numeric descriptor ID.
    #[must_use]
    pub fn get(self, id: MetricId) -> Option<&'static MetricDescriptor> {
        self.descriptors
            .iter()
            .find(|descriptor| descriptor.id == id)
    }

    /// Deserializes and validates a metric batch against this registry.
    ///
    /// # Errors
    ///
    /// Returns a deserializer error when the batch exceeds its bound, references an unknown or
    /// duplicate metric ID, or supplies a value with the wrong metric kind.
    pub fn deserialize_batch<'de, D, const MAX: usize>(
        self,
        deserializer: D,
    ) -> Result<MetricBatch<MAX>, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire<const MAX: usize> {
            sequence: Sequence,
            samples: BoundedVec<MetricSample, MAX>,
        }

        let wire = Wire::deserialize(deserializer)?;
        MetricBatch::from_bounded(self, wire.sequence, wire.samples).map_err(D::Error::custom)
    }
}

/// A static metric registry validation error.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum MetricRegistryError {
    #[error("metric {id:?} has an empty name")]
    EmptyName { id: MetricId },
    #[error("metric ID {id:?} is duplicated")]
    DuplicateId { id: MetricId },
    #[error("metric name {name:?} is duplicated")]
    DuplicateName { name: &'static str },
}

/// A numeric metric value.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum MetricValue {
    Counter(u64),
    Gauge(f64),
}

impl MetricValue {
    const fn kind(self) -> MetricKind {
        match self {
            Self::Counter(_) => MetricKind::Counter,
            Self::Gauge(_) => MetricKind::Gauge,
        }
    }
}

/// One metric sample identified by its static descriptor ID.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct MetricSample {
    pub metric_id: MetricId,
    pub value: MetricValue,
}

/// An ordered, bounded batch of metric samples.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct MetricBatch<const MAX: usize> {
    sequence: Sequence,
    samples: BoundedVec<MetricSample, MAX>,
}

impl<const MAX: usize> MetricBatch<MAX> {
    /// Validates all samples against `registry` before cloning the supplied slice.
    ///
    /// # Errors
    ///
    /// Returns [`MetricBatchError`] when the sample count exceeds `MAX`, an ID is unknown or
    /// duplicated, or a value does not match its descriptor's kind.
    pub fn from_slice(
        registry: MetricRegistry,
        sequence: Sequence,
        samples: &[MetricSample],
    ) -> Result<Self, MetricBatchError> {
        Self::from_bounded(registry, sequence, BoundedVec::from_slice(samples)?)
    }

    fn from_bounded(
        registry: MetricRegistry,
        sequence: Sequence,
        samples: BoundedVec<MetricSample, MAX>,
    ) -> Result<Self, MetricBatchError> {
        Self::validate(registry, &samples)?;
        Ok(Self { sequence, samples })
    }

    fn validate(
        registry: MetricRegistry,
        samples: &[MetricSample],
    ) -> Result<(), MetricBatchError> {
        for (index, sample) in samples.iter().enumerate() {
            let descriptor =
                registry
                    .get(sample.metric_id)
                    .ok_or(MetricBatchError::UnknownMetric {
                        id: sample.metric_id,
                    })?;
            if samples[..index]
                .iter()
                .any(|existing| existing.metric_id == sample.metric_id)
            {
                return Err(MetricBatchError::DuplicateMetric {
                    id: sample.metric_id,
                });
            }
            let actual = sample.value.kind();
            if descriptor.kind != actual {
                return Err(MetricBatchError::KindMismatch {
                    id: sample.metric_id,
                    expected: descriptor.kind,
                    actual,
                });
            }
        }
        Ok(())
    }

    /// Returns the batch sequence.
    #[must_use]
    pub const fn sequence(&self) -> Sequence {
        self.sequence
    }

    /// Returns the validated metric samples.
    #[must_use]
    pub fn samples(&self) -> &[MetricSample] {
        self.samples.as_slice()
    }
}

/// A metric batch that does not agree with its registry or configured bound.
#[derive(Clone, Copy, Debug, Error, PartialEq)]
pub enum MetricBatchError {
    #[error(transparent)]
    Bound(#[from] BoundError),
    #[error("metric ID {id:?} is not registered")]
    UnknownMetric { id: MetricId },
    #[error("metric ID {id:?} occurs more than once in a batch")]
    DuplicateMetric { id: MetricId },
    #[error("metric ID {id:?} expects {expected:?}, but received {actual:?}")]
    KindMismatch {
        id: MetricId,
        expected: MetricKind,
        actual: MetricKind,
    },
}
