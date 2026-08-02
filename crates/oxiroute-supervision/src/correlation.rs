use std::{collections::BTreeMap, num::NonZeroU64};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{BoundedVec, Epoch, RequestId};

/// A nonzero generation assigned when a request ID is inserted.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(transparent)]
pub struct CorrelationGeneration(NonZeroU64);

impl CorrelationGeneration {
    /// Returns the generation value.
    #[must_use]
    pub const fn get(self) -> u64 {
        self.0.get()
    }
}

/// An unambiguous request correlation that remains unique when request IDs are reused.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
pub struct CorrelationToken {
    request_id: RequestId,
    generation: CorrelationGeneration,
}

impl CorrelationToken {
    /// Returns the caller-selected request ID.
    #[must_use]
    pub const fn request_id(self) -> RequestId {
        self.request_id
    }

    /// Returns the table-assigned request generation.
    #[must_use]
    pub const fn generation(self) -> CorrelationGeneration {
        self.generation
    }
}

/// Caller-owned data retained while an RPC is outstanding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingRequest<T> {
    pub deadline: Epoch,
    pub value: T,
}

/// A correlation table operation failure.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum CorrelationError {
    #[error("request {request_id} is already pending")]
    Duplicate { request_id: RequestId },
    #[error("correlation table reached its limit of {maximum} requests")]
    Full { maximum: usize },
    #[error("correlation token {token:?} is not pending")]
    Unknown { token: CorrelationToken },
    #[error("correlation generations are exhausted")]
    GenerationExhausted,
}

/// A deterministic, bounded RPC correlation table with caller-supplied time.
#[derive(Clone, Debug)]
pub struct CorrelationTable<T, const MAX: usize> {
    pending: BTreeMap<RequestId, (CorrelationToken, PendingRequest<T>)>,
    next_generation: u64,
}

impl<T, const MAX: usize> CorrelationTable<T, MAX> {
    /// Creates an empty table without allocating.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pending: BTreeMap::new(),
            next_generation: 1,
        }
    }

    /// Returns the number of pending requests.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Returns whether the table has no pending requests.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    /// Inserts a request without replacing an existing correlation.
    ///
    /// # Errors
    ///
    /// Returns [`CorrelationError::Duplicate`] for an existing request ID, or
    /// [`CorrelationError::Full`] when `MAX` requests are already pending.
    pub fn insert(
        &mut self,
        request_id: RequestId,
        deadline: Epoch,
        value: T,
    ) -> Result<CorrelationToken, CorrelationError> {
        if self.pending.contains_key(&request_id) {
            return Err(CorrelationError::Duplicate { request_id });
        }
        if self.pending.len() == MAX {
            return Err(CorrelationError::Full { maximum: MAX });
        }
        let generation =
            NonZeroU64::new(self.next_generation).ok_or(CorrelationError::GenerationExhausted)?;
        let token = CorrelationToken {
            request_id,
            generation: CorrelationGeneration(generation),
        };
        self.next_generation = self.next_generation.checked_add(1).unwrap_or(0);
        self.pending
            .insert(request_id, (token, PendingRequest { deadline, value }));
        Ok(token)
    }

    /// Completes and removes a pending request.
    ///
    /// # Errors
    ///
    /// Returns [`CorrelationError::Unknown`] when no request has this ID.
    pub fn complete(
        &mut self,
        token: CorrelationToken,
    ) -> Result<PendingRequest<T>, CorrelationError> {
        match self.pending.get(&token.request_id) {
            Some((pending_token, _)) if *pending_token == token => {}
            _ => return Err(CorrelationError::Unknown { token }),
        }
        self.pending
            .remove(&token.request_id)
            .map(|(_, pending)| pending)
            .ok_or(CorrelationError::Unknown { token })
    }

    /// Removes all requests whose deadline is less than or equal to `now`.
    ///
    /// Results are ordered by request ID and can contain at most `MAX` entries.
    #[must_use]
    pub fn expire(&mut self, now: Epoch) -> BoundedVec<(CorrelationToken, PendingRequest<T>), MAX> {
        let expired_ids: Vec<_> = self
            .pending
            .iter()
            .filter_map(|(request_id, (_, pending))| {
                (pending.deadline <= now).then_some(*request_id)
            })
            .collect();
        let expired = expired_ids
            .into_iter()
            .filter_map(|request_id| self.pending.remove(&request_id))
            .collect::<Vec<_>>();
        BoundedVec::from_vec_within_bound(expired)
    }
}

impl<T, const MAX: usize> Default for CorrelationTable<T, MAX> {
    fn default() -> Self {
        Self::new()
    }
}
