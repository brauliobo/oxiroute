use std::time::Duration;

use serde::{Deserialize, Serialize};

/// A side-effect-free lifecycle operation requested from a runtime owner.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleOperation {
    Reload,
    Rollback,
    Drain,
    Shutdown,
}

/// A lifecycle intent carrying the revision precondition observed by its caller.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LifecycleRequest<R> {
    operation: LifecycleOperation,
    expected_revision: R,
}

impl<R> LifecycleRequest<R> {
    /// Creates a lifecycle intent for one expected revision.
    #[must_use]
    pub const fn new(operation: LifecycleOperation, expected_revision: R) -> Self {
        Self {
            operation,
            expected_revision,
        }
    }

    /// Returns the requested lifecycle operation.
    #[must_use]
    pub const fn operation(&self) -> LifecycleOperation {
        self.operation
    }

    /// Returns the caller's revision precondition.
    #[must_use]
    pub const fn expected_revision(&self) -> &R {
        &self.expected_revision
    }

    /// Consumes the request and returns its revision precondition.
    #[must_use]
    pub fn into_expected_revision(self) -> R {
        self.expected_revision
    }
}

/// Mode-neutral lifecycle control port implemented by direct and supervised runtimes.
pub trait LifecycleControl {
    /// Revision type used for optimistic lifecycle preconditions.
    type Revision: Clone;
    /// Read-only status projection returned by the runtime owner.
    type Status;
    /// Successful mutation projection returned by the runtime owner.
    type Outcome;
    /// Mutation failure projection returned by the runtime owner.
    type Error;

    /// Returns the current lifecycle status without requesting a mutation.
    fn status(&self) -> Self::Status;

    /// Executes one lifecycle request against the runtime owner.
    ///
    /// # Errors
    ///
    /// Returns the runtime owner's operation-specific failure projection.
    fn execute(
        &self,
        request: LifecycleRequest<Self::Revision>,
        timeout: Option<Duration>,
    ) -> Result<Self::Outcome, Self::Error>;

    /// Creates a reload request for `expected` without performing I/O.
    fn request_reload(&self, expected: &Self::Revision) -> LifecycleRequest<Self::Revision> {
        LifecycleRequest::new(LifecycleOperation::Reload, expected.clone())
    }

    /// Creates a rollback request for `expected` without performing I/O.
    fn request_rollback(&self, expected: &Self::Revision) -> LifecycleRequest<Self::Revision> {
        LifecycleRequest::new(LifecycleOperation::Rollback, expected.clone())
    }

    /// Creates a drain request for `expected` without performing I/O.
    fn request_drain(&self, expected: &Self::Revision) -> LifecycleRequest<Self::Revision> {
        LifecycleRequest::new(LifecycleOperation::Drain, expected.clone())
    }

    /// Creates a shutdown request for `expected` without performing I/O.
    fn request_shutdown(&self, expected: &Self::Revision) -> LifecycleRequest<Self::Revision> {
        LifecycleRequest::new(LifecycleOperation::Shutdown, expected.clone())
    }
}
