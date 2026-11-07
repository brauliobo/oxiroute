#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone, Debug)]
pub struct AcmeOperation {
    cancellation: Dns01Cancellation,
    deadline: Option<std::time::Instant>,
}

impl Default for AcmeOperation {
    fn default() -> Self {
        Self {
            cancellation: Dns01Cancellation::new(),
            deadline: None,
        }
    }
}

impl AcmeOperation {
    #[must_use]
    pub fn with_deadline(cancellation: Dns01Cancellation, deadline: std::time::Instant) -> Self {
        Self {
            cancellation,
            deadline: Some(deadline),
        }
    }

    #[must_use]
    pub const fn cancellation(&self) -> &Dns01Cancellation {
        &self.cancellation
    }

    /// Checks cancellation and the operation deadline.
    ///
    /// # Errors
    ///
    /// Returns [`AcmeError::Cancelled`] after cancellation or deadline expiry.
    pub fn check(&self) -> Result<(), AcmeError> {
        if self.cancellation.is_cancelled()
            || self
                .deadline
                .is_some_and(|deadline| std::time::Instant::now() >= deadline)
        {
            Err(AcmeError::Cancelled)
        } else {
            Ok(())
        }
    }

    #[must_use]
    pub fn remaining(&self, maximum: std::time::Duration) -> std::time::Duration {
        self.deadline.map_or(maximum, |deadline| {
            deadline
                .saturating_duration_since(std::time::Instant::now())
                .min(maximum)
        })
    }

    /// Waits interruptibly for at most the requested duration and operation deadline.
    ///
    /// # Errors
    ///
    /// Returns [`AcmeError::Cancelled`] when cancellation or the deadline interrupts the wait.
    pub fn wait(&self, duration: std::time::Duration) -> Result<(), AcmeError> {
        self.check()?;
        let wait = self.remaining(duration);
        if wait.is_zero() || self.cancellation.wait_timeout(wait) {
            return Err(AcmeError::Cancelled);
        }
        self.check()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AcmeFailureClass {
    Permanent,
    Retryable,
}

#[derive(Debug, thiserror::Error)]
pub enum AcmeError {
    #[error("ACME directory URL is invalid or not HTTPS")]
    InvalidDirectoryUrl,
    #[error("ACME endpoint is outside the configured outbound origin policy")]
    EndpointOutsidePolicy,
    #[error("ACME endpoint uses an untrusted redirect")]
    UntrustedRedirect,
    #[error("ACME private or local origin requires an explicit development allowlist")]
    PrivateOriginRequiresAllowlist,
    #[error("ACME response exceeds the {limit}-byte bound")]
    ResponseTooLarge { limit: usize },
    #[error("ACME transport failed")]
    Transport(#[source] TransportError),
    #[error("ACME response returned unexpected HTTP status {status}")]
    UnexpectedStatus { status: u16 },
    #[error("ACME response is malformed")]
    MalformedResponse,
    #[error("ACME response is missing a required field")]
    MissingField,
    #[error("ACME problem type `{problem_type}` was returned")]
    Problem { problem_type: String },
    #[error("ACME replay nonce is missing")]
    MissingNonce,
    #[error("ACME replay nonce pool is exhausted")]
    NoncePoolExhausted,
    #[error("ACME returned badNonce after its one permitted retry")]
    BadNonceRetryExhausted,
    #[error("ACME account registration is ambiguous and was not replayed")]
    AccountRegistrationAmbiguous,
    #[error("ACME account contact policy is invalid")]
    InvalidAccountRequest,
    #[error("ACME account is not usable: {status}")]
    AccountNotUsable { status: String },
    #[error("ACME terms of service must be explicitly agreed")]
    TermsNotAgreed,
    #[error("ACME directory requires external account binding")]
    ExternalAccountBindingRequired,
    #[error("ACME identifier set is invalid or unsupported")]
    UnsupportedIdentifier,
    #[error("ACME IP identifiers are unsupported by the managed issuance path")]
    IpIdentifierUnsupported,
    #[error("ACME order identifiers do not match the requested set")]
    OrderIdentifiersMismatch,
    #[error("ACME order returned an unsupported state: {status}")]
    InvalidOrderState { status: String },
    #[error("ACME authorization has no supported challenge")]
    UnsupportedChallenge,
    #[error("ACME polling exceeded its bounded deadline")]
    PollTimeout,
    #[error("ACME operation was cancelled")]
    Cancelled,
    #[error("ACME retry guidance is invalid")]
    InvalidRetryAfter,
    #[error("ACME request exceeds the {limit}-byte bound")]
    RequestTooLarge { limit: usize },
    #[error("ACME account key could not be generated or used")]
    Key(#[source] ErrorStack),
    #[error("ACME directory does not advertise certificate revocation")]
    RevocationUnsupported,
    #[error("ACME directory does not advertise account key rollover")]
    KeyChangeUnsupported,
    #[error("ACME certificate PEM is invalid")]
    InvalidCertificate,
    #[error("ACME revocation reason is invalid")]
    InvalidRevocationReason,
    #[error("ACME Renewal Information response is invalid")]
    InvalidRenewalInformation,
    #[error("ACME JWS signature could not be created")]
    Signature(#[source] ErrorStack),
    #[error("ACME CSR could not be created")]
    Csr(#[source] ErrorStack),
}

impl AcmeError {
    #[must_use]
    pub(super) fn failure_class(&self) -> AcmeFailureClass {
        match self {
            Self::Transport(_)
            | Self::MissingNonce
            | Self::NoncePoolExhausted
            | Self::BadNonceRetryExhausted
            | Self::PollTimeout
            | Self::InvalidRetryAfter => AcmeFailureClass::Retryable,
            Self::UnexpectedStatus { status }
                if matches!(*status, 408 | 425 | 429 | 500 | 502 | 503 | 504) =>
            {
                AcmeFailureClass::Retryable
            }
            Self::Problem { problem_type } if retryable_problem_type(problem_type) => {
                AcmeFailureClass::Retryable
            }
            _ => AcmeFailureClass::Permanent,
        }
    }

    #[must_use]
    pub fn is_retryable(&self) -> bool {
        self.failure_class() == AcmeFailureClass::Retryable
    }
}

fn retryable_problem_type(problem_type: &str) -> bool {
    matches!(
        problem_type.rsplit(':').next(),
        Some("badNonce" | "connection" | "dns" | "rateLimited" | "serverInternal" | "tls")
    )
}
