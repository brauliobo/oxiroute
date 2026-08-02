use std::time::Duration;

use thiserror::Error;

const MAX_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Validated deadlines used by the master state machine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MasterConfig {
    adoption: Duration,
    quiesce: Duration,
    activation: Duration,
    drain: Duration,
    shutdown: Duration,
}

impl MasterConfig {
    /// Validates all phase deadlines.
    ///
    /// # Errors
    ///
    /// Returns an error when any timeout is zero or exceeds one hour.
    pub fn new(
        adoption_timeout: Duration,
        quiesce_timeout: Duration,
        activation_timeout: Duration,
        drain_timeout: Duration,
        shutdown_timeout: Duration,
    ) -> Result<Self, ConfigError> {
        for (phase, timeout) in [
            ("listener adoption", adoption_timeout),
            ("quiesce", quiesce_timeout),
            ("activation", activation_timeout),
            ("drain", drain_timeout),
            ("shutdown", shutdown_timeout),
        ] {
            if timeout.is_zero() || timeout > MAX_TIMEOUT {
                return Err(ConfigError::InvalidTimeout {
                    phase,
                    maximum: MAX_TIMEOUT,
                });
            }
        }
        Ok(Self {
            adoption: adoption_timeout,
            quiesce: quiesce_timeout,
            activation: activation_timeout,
            drain: drain_timeout,
            shutdown: shutdown_timeout,
        })
    }

    /// Returns the listener-adoption acknowledgement timeout.
    #[must_use]
    pub const fn adoption_timeout(self) -> Duration {
        self.adoption
    }

    /// Returns the active quiescence acknowledgement timeout.
    #[must_use]
    pub const fn quiesce_timeout(self) -> Duration {
        self.quiesce
    }

    /// Returns the initial, candidate, and rollback activation acknowledgement timeout.
    #[must_use]
    pub const fn activation_timeout(self) -> Duration {
        self.activation
    }

    /// Returns the retired-worker drain acknowledgement timeout.
    #[must_use]
    pub const fn drain_timeout(self) -> Duration {
        self.drain
    }

    /// Returns the cooperative worker shutdown timeout before forced termination.
    #[must_use]
    pub const fn shutdown_timeout(self) -> Duration {
        self.shutdown
    }
}

/// Invalid master configuration.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ConfigError {
    /// One timeout was outside the supported bound.
    #[error("{phase} timeout must be nonzero and no greater than {maximum:?}")]
    InvalidTimeout {
        phase: &'static str,
        maximum: Duration,
    },
}
