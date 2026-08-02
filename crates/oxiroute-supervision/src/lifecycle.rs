use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Runtime lifecycle of one supervised service instance.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifecycle {
    Spawned,
    Handshaking,
    Preparing,
    Ready,
    Activating,
    Active,
    Quiescing,
    Reactivating,
    Draining,
    Snapshotting,
    Stopping,
    Stopped,
    Failed,
}

impl Lifecycle {
    /// Returns whether an explicit transition from this state to `next` is valid.
    #[must_use]
    pub const fn can_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Spawned, Self::Handshaking)
                | (Self::Handshaking, Self::Preparing)
                | (Self::Preparing, Self::Ready)
                | (Self::Ready, Self::Activating)
                | (Self::Activating | Self::Reactivating, Self::Active)
                | (Self::Active, Self::Quiescing)
                | (Self::Quiescing, Self::Reactivating | Self::Draining)
                | (Self::Draining, Self::Snapshotting)
                | (
                    Self::Snapshotting
                        | Self::Failed
                        | Self::Spawned
                        | Self::Handshaking
                        | Self::Preparing
                        | Self::Ready
                        | Self::Activating
                        | Self::Active
                        | Self::Quiescing
                        | Self::Reactivating,
                    Self::Stopping
                )
                | (Self::Stopping, Self::Stopped)
                | (
                    Self::Spawned
                        | Self::Handshaking
                        | Self::Preparing
                        | Self::Ready
                        | Self::Activating
                        | Self::Active
                        | Self::Quiescing
                        | Self::Reactivating
                        | Self::Draining
                        | Self::Snapshotting
                        | Self::Stopping,
                    Self::Failed
                )
        )
    }

    /// Applies a valid lifecycle transition.
    ///
    /// # Errors
    ///
    /// Returns [`TransitionError`] when the transition is not explicitly allowed.
    pub const fn transition(self, next: Self) -> Result<Self, TransitionError> {
        if self.can_transition_to(next) {
            Ok(next)
        } else {
            Err(TransitionError {
                from: self,
                to: next,
            })
        }
    }

    /// Returns whether this state admits no further normal work.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Stopped)
    }
}

/// An invalid lifecycle transition.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("invalid lifecycle transition from {from:?} to {to:?}")]
pub struct TransitionError {
    pub from: Lifecycle,
    pub to: Lifecycle,
}
