use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use crate::SecretBytes;

pub const MAX_DNS01_PROVIDER_NAME_BYTES: usize = 64;
pub const MAX_DNS01_CREDENTIAL_REFERENCE_BYTES: usize = 4_096;
pub const MAX_DNS01_CREDENTIAL_BYTES: usize = 64 * 1024;
pub const MAX_DNS01_RECORD_ID_BYTES: usize = 256;
pub const MAX_DNS01_OPERATION_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// An opaque reference to provider credentials. The referenced secret is never held here.
#[derive(Clone, Eq, PartialEq)]
pub struct Dns01CredentialReference(String);

impl Dns01CredentialReference {
    /// Creates a bounded, opaque secret reference without reading the referenced secret.
    ///
    /// # Errors
    ///
    /// Returns an error when the reference is empty, oversized, or contains control bytes.
    pub fn new(reference: impl Into<String>) -> Result<Self, Dns01ProviderError> {
        let reference = reference.into();
        if reference.is_empty()
            || reference.len() > MAX_DNS01_CREDENTIAL_REFERENCE_BYTES
            || !reference.is_ascii()
            || reference.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(Dns01ProviderError::InvalidCredentialReference);
        }
        Ok(Self(reference))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Dns01CredentialReference {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Dns01CredentialReference(REDACTED)")
    }
}

/// Bounded provider credentials. The value is intentionally absent from `Debug`, errors, and
/// serialized state.
#[derive(Clone, Default)]
pub struct Dns01Credentials(SecretBytes);

impl Dns01Credentials {
    /// Wraps one already bounded secret value for a provider call.
    ///
    /// # Errors
    ///
    /// Returns an error when the value is empty or exceeds the provider credential bound.
    pub fn new(bytes: impl Into<Vec<u8>>) -> Result<Self, Dns01ProviderError> {
        let bytes = bytes.into();
        if bytes.is_empty() || bytes.len() > MAX_DNS01_CREDENTIAL_BYTES {
            return Err(Dns01ProviderError::InvalidCredentials);
        }
        Ok(Self(SecretBytes::new(bytes)))
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }
}

impl fmt::Debug for Dns01Credentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("Dns01Credentials(REDACTED)")
    }
}

/// Exact ACME DNS-01 material passed to a provider. The TXT value is never printable.
#[derive(Clone, Eq, PartialEq)]
pub struct Dns01Challenge {
    identifier: String,
    challenge_url: String,
    record_name: String,
    record_value: String,
}

impl Dns01Challenge {
    /// Creates one bounded challenge description.
    ///
    /// # Errors
    ///
    /// Returns an error when the identifier, URL, record name, or TXT value is malformed.
    pub fn new(
        identifier: impl Into<String>,
        challenge_url: impl Into<String>,
        record_name: impl Into<String>,
        record_value: impl Into<String>,
    ) -> Result<Self, Dns01ProviderError> {
        let identifier = identifier.into();
        let challenge_url = challenge_url.into();
        let record_name = record_name.into();
        let record_value = record_value.into();
        if !valid_dns_identifier(&identifier)
            || challenge_url.is_empty()
            || challenge_url.len() > 2_048
            || !challenge_url.is_ascii()
            || challenge_url.bytes().any(|byte| byte.is_ascii_control())
            || !valid_dns_record_name(&record_name)
            || record_value.is_empty()
            || record_value.len() > 512
            || !record_value.is_ascii()
            || record_value.bytes().any(|byte| byte.is_ascii_whitespace())
        {
            return Err(Dns01ProviderError::InvalidRecord);
        }
        Ok(Self {
            identifier,
            challenge_url,
            record_name,
            record_value,
        })
    }

    #[must_use]
    pub fn identifier(&self) -> &str {
        &self.identifier
    }

    #[must_use]
    pub fn challenge_url(&self) -> &str {
        &self.challenge_url
    }

    #[must_use]
    pub fn record_name(&self) -> &str {
        &self.record_name
    }

    #[must_use]
    pub fn record_value(&self) -> &str {
        &self.record_value
    }
}

impl fmt::Debug for Dns01Challenge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Dns01Challenge")
            .field("identifier", &self.identifier)
            .field("challenge_url", &self.challenge_url)
            .field("record_name", &self.record_name)
            .field("record_value", &"REDACTED")
            .finish()
    }
}

/// Cooperative cancellation shared by provider operations and their caller.
#[derive(Clone, Debug, Default)]
pub struct Dns01Cancellation(Arc<AtomicBool>);

impl Dns01Cancellation {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// Bounded operation context supplied to every provider call. Providers must check it before
/// every remote operation and while waiting for propagation.
#[derive(Clone, Debug)]
pub struct Dns01Operation {
    deadline: Instant,
    cancellation: Dns01Cancellation,
}

impl Dns01Operation {
    /// Creates an operation with a bounded deadline and a fresh cancellation token.
    ///
    /// # Errors
    ///
    /// Returns an error when the timeout is zero or exceeds the global bound.
    pub fn new(timeout: Duration) -> Result<Self, Dns01ProviderError> {
        Self::with_cancellation(timeout, Dns01Cancellation::new())
    }

    /// Creates an operation using a caller-owned cancellation token.
    ///
    /// # Errors
    ///
    /// Returns an error when the timeout is zero or exceeds the global bound.
    pub fn with_cancellation(
        timeout: Duration,
        cancellation: Dns01Cancellation,
    ) -> Result<Self, Dns01ProviderError> {
        if timeout.is_zero() || timeout > MAX_DNS01_OPERATION_TIMEOUT {
            return Err(Dns01ProviderError::InvalidOperationTimeout);
        }
        Ok(Self {
            deadline: Instant::now() + timeout,
            cancellation,
        })
    }

    #[must_use]
    pub fn cancellation(&self) -> &Dns01Cancellation {
        &self.cancellation
    }

    #[must_use]
    pub fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    /// Checks cancellation and the operation deadline.
    ///
    /// # Errors
    ///
    /// Returns a categorical cancellation or timeout error without provider detail.
    pub fn check(&self) -> Result<(), Dns01ProviderError> {
        if self.cancellation.is_cancelled() {
            return Err(Dns01ProviderError::Cancelled);
        }
        if Instant::now() >= self.deadline {
            return Err(Dns01ProviderError::Timeout);
        }
        Ok(())
    }
}

/// The exact TXT record created by one provider operation.
pub struct Dns01Record {
    provider: String,
    challenge_url: String,
    record_name: String,
    record_value: SecretBytes,
    provider_record_id: String,
}

impl Dns01Record {
    /// Builds a provider record result. The orchestrator validates it against the requested
    /// challenge before allowing the ACME authorization to continue.
    ///
    /// # Errors
    ///
    /// Returns an error when an identity, value, or provider record ID is unbounded or malformed.
    pub fn new(
        provider: impl Into<String>,
        challenge_url: impl Into<String>,
        record_name: impl Into<String>,
        record_value: impl Into<Vec<u8>>,
        provider_record_id: impl Into<String>,
    ) -> Result<Self, Dns01ProviderError> {
        let provider = provider.into();
        let challenge_url = challenge_url.into();
        let record_name = record_name.into();
        let record_value = record_value.into();
        let provider_record_id = provider_record_id.into();
        validate_provider_name(&provider)?;
        if challenge_url.is_empty()
            || challenge_url.len() > 2_048
            || !challenge_url.is_ascii()
            || challenge_url.bytes().any(|byte| byte.is_ascii_control())
            || !valid_dns_record_name(&record_name)
            || record_value.is_empty()
            || record_value.len() > 512
            || !record_value.is_ascii()
            || record_value.iter().any(u8::is_ascii_whitespace)
            || provider_record_id.is_empty()
            || provider_record_id.len() > MAX_DNS01_RECORD_ID_BYTES
            || !provider_record_id.is_ascii()
            || provider_record_id
                .bytes()
                .any(|byte| byte.is_ascii_control())
        {
            return Err(Dns01ProviderError::InvalidRecord);
        }
        Ok(Self {
            provider,
            challenge_url,
            record_name,
            record_value: SecretBytes::new(record_value),
            provider_record_id,
        })
    }

    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    #[must_use]
    pub fn challenge_url(&self) -> &str {
        &self.challenge_url
    }

    #[must_use]
    pub fn record_name(&self) -> &str {
        &self.record_name
    }

    #[must_use]
    pub fn record_value(&self) -> &[u8] {
        self.record_value.as_bytes()
    }

    #[must_use]
    pub fn provider_record_id(&self) -> &str {
        &self.provider_record_id
    }

    #[must_use]
    pub fn matches(&self, challenge: &Dns01Challenge, provider: &str) -> bool {
        self.provider == provider
            && self.challenge_url == challenge.challenge_url()
            && self.record_name == challenge.record_name()
            && self.record_value.as_bytes() == challenge.record_value().as_bytes()
    }
}

impl fmt::Debug for Dns01Record {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Dns01Record")
            .field("provider", &self.provider)
            .field("challenge_url", &self.challenge_url)
            .field("record_name", &self.record_name)
            .field("record_value", &"REDACTED")
            .field("provider_record_id", &self.provider_record_id)
            .finish()
    }
}

/// Narrow in-process DNS-01 provider contract. Implementations must be statically linked and
/// registered by exact provider name; dynamic loading and shell hooks are not part of this API.
pub trait Dns01Provider: Send + Sync {
    fn name(&self) -> &str;

    /// Creates exactly the requested TXT value and returns its provider-owned identity.
    ///
    /// # Errors
    ///
    /// Implementations return only categorical provider errors and must not include credentials or
    /// challenge values in them.
    fn create_txt_record(
        &self,
        challenge: &Dns01Challenge,
        credentials: &Dns01Credentials,
        operation: &Dns01Operation,
    ) -> Result<Dns01Record, Dns01ProviderError>;

    /// Waits until the exact TXT value is visible to the provider's propagation policy.
    ///
    /// # Errors
    ///
    /// Returns a bounded propagation, timeout, or cancellation error without provider detail.
    fn wait_for_propagation(
        &self,
        challenge: &Dns01Challenge,
        record: &Dns01Record,
        credentials: &Dns01Credentials,
        operation: &Dns01Operation,
    ) -> Result<(), Dns01ProviderError>;

    /// Removes only the exact record identity returned by `create_txt_record`.
    ///
    /// # Errors
    ///
    /// Implementations return a cleanup error when the exact record cannot be removed.
    fn cleanup_txt_record(
        &self,
        record: &Dns01Record,
        credentials: &Dns01Credentials,
        operation: &Dns01Operation,
    ) -> Result<(), Dns01ProviderError>;
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Dns01ProviderAllowlist {
    names: BTreeSet<String>,
}

impl Dns01ProviderAllowlist {
    /// Creates an exact provider-name allowlist.
    ///
    /// # Errors
    ///
    /// Returns an error when a name is not a bounded static provider identifier.
    pub fn new(names: impl IntoIterator<Item = String>) -> Result<Self, Dns01ProviderError> {
        let mut allowed = BTreeSet::new();
        for name in names {
            validate_provider_name(&name)?;
            allowed.insert(name);
        }
        Ok(Self { names: allowed })
    }

    #[must_use]
    pub fn permits(&self, name: &str) -> bool {
        self.names.contains(name)
    }
}

/// Registry of statically linked providers. Its allowlist is the only provider discovery surface.
#[derive(Clone, Default)]
pub struct Dns01ProviderRegistry {
    allowlist: Dns01ProviderAllowlist,
    providers: BTreeMap<String, Arc<dyn Dns01Provider>>,
}

impl Dns01ProviderRegistry {
    /// Creates an empty registry with the supplied exact allowlist.
    ///
    /// # Errors
    ///
    /// Returns an error when an allowlist name is invalid.
    pub fn new(names: impl IntoIterator<Item = String>) -> Result<Self, Dns01ProviderError> {
        Ok(Self {
            allowlist: Dns01ProviderAllowlist::new(names)?,
            providers: BTreeMap::new(),
        })
    }

    /// Registers one statically linked provider if its exact name is allowlisted.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid or unallowlisted provider name, or duplicate registration.
    pub fn register<P>(&mut self, provider: P) -> Result<(), Dns01ProviderError>
    where
        P: Dns01Provider + 'static,
    {
        let name = provider.name().to_owned();
        validate_provider_name(&name)?;
        if !self.allowlist.permits(&name) {
            return Err(Dns01ProviderError::ProviderNotAllowlisted);
        }
        if self.providers.contains_key(&name) {
            return Err(Dns01ProviderError::DuplicateProvider);
        }
        self.providers.insert(name, Arc::new(provider));
        Ok(())
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn Dns01Provider>> {
        self.providers.get(name).cloned()
    }

    #[must_use]
    pub fn permits(&self, name: &str) -> bool {
        self.allowlist.permits(name)
    }
}

impl fmt::Debug for Dns01ProviderRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Dns01ProviderRegistry")
            .field("allowlisted", &self.allowlist.names)
            .field("registered", &self.providers.keys())
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Dns01ProviderError {
    InvalidCredentialReference,
    InvalidCredentials,
    InvalidOperationTimeout,
    InvalidProviderName,
    InvalidRecord,
    ProviderNotAllowlisted,
    DuplicateProvider,
    UnsupportedProvider,
    ProviderFailed,
    CleanupFailed,
    Timeout,
    Cancelled,
}

impl fmt::Display for Dns01ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidCredentialReference => "DNS-01 credential reference is invalid",
            Self::InvalidCredentials => "DNS-01 credentials are invalid or exceed the bound",
            Self::InvalidOperationTimeout => "DNS-01 operation timeout is outside its bound",
            Self::InvalidProviderName => "DNS-01 provider name is invalid",
            Self::InvalidRecord => "DNS-01 provider returned an invalid record",
            Self::ProviderNotAllowlisted => "DNS-01 provider is not allowlisted",
            Self::DuplicateProvider => "DNS-01 provider is already registered",
            Self::UnsupportedProvider => "DNS-01 provider is unsupported",
            Self::ProviderFailed => "DNS-01 provider operation failed",
            Self::CleanupFailed => "DNS-01 provider cleanup failed",
            Self::Timeout => "DNS-01 provider operation timed out",
            Self::Cancelled => "DNS-01 provider operation was cancelled",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for Dns01ProviderError {}

fn validate_provider_name(name: &str) -> Result<(), Dns01ProviderError> {
    if name.is_empty()
        || name.len() > MAX_DNS01_PROVIDER_NAME_BYTES
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || name.starts_with('.')
        || name.ends_with('.')
    {
        return Err(Dns01ProviderError::InvalidProviderName);
    }
    Ok(())
}

fn valid_dns_identifier(name: &str) -> bool {
    let name = name.strip_prefix("*.").unwrap_or(name);
    !name.is_empty()
        && name.len() <= 253
        && name.is_ascii()
        && !name.ends_with('.')
        && !name.contains('*')
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}

fn valid_dns_record_name(name: &str) -> bool {
    name.len() <= 253
        && name.is_ascii()
        && !name.ends_with('.')
        && name.split('.').enumerate().all(|(index, label)| {
            !label.is_empty()
                && label.len() <= 63
                && (index == 0 && label == "_acme-challenge"
                    || index != 0
                        && label
                            .bytes()
                            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                        && !label.starts_with('-')
                        && !label.ends_with('-'))
        })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Default)]
    struct FakeProvider {
        created: Mutex<Vec<String>>,
        cleaned: Mutex<Vec<String>>,
    }

    impl Dns01Provider for FakeProvider {
        fn name(&self) -> &'static str {
            "fake"
        }

        fn create_txt_record(
            &self,
            challenge: &Dns01Challenge,
            credentials: &Dns01Credentials,
            operation: &Dns01Operation,
        ) -> Result<Dns01Record, Dns01ProviderError> {
            operation.check()?;
            assert_eq!(credentials.as_bytes(), b"provider-secret");
            self.created
                .lock()
                .expect("created lock")
                .push(challenge.record_name().into());
            Dns01Record::new(
                "fake",
                challenge.challenge_url(),
                challenge.record_name(),
                challenge.record_value().as_bytes().to_vec(),
                "record-1",
            )
        }

        fn cleanup_txt_record(
            &self,
            record: &Dns01Record,
            credentials: &Dns01Credentials,
            operation: &Dns01Operation,
        ) -> Result<(), Dns01ProviderError> {
            operation.check()?;
            assert_eq!(credentials.as_bytes(), b"provider-secret");
            self.cleaned
                .lock()
                .expect("cleaned lock")
                .push(record.provider_record_id().into());
            Ok(())
        }

        fn wait_for_propagation(
            &self,
            challenge: &Dns01Challenge,
            record: &Dns01Record,
            credentials: &Dns01Credentials,
            operation: &Dns01Operation,
        ) -> Result<(), Dns01ProviderError> {
            operation.check()?;
            assert_eq!(record.record_name(), challenge.record_name());
            assert_eq!(credentials.as_bytes(), b"provider-secret");
            Ok(())
        }
    }

    fn challenge() -> Dns01Challenge {
        Dns01Challenge::new(
            "*.example.test",
            "https://acme.test/challenge/1",
            "_acme-challenge.example.test",
            "txt-value",
        )
        .expect("challenge")
    }

    #[test]
    fn allowlist_and_fake_provider_keep_exact_record_identity() {
        let provider = Arc::new(FakeProvider::default());
        let mut registry = Dns01ProviderRegistry::new(["fake".into()]).expect("registry");
        registry
            .register(FakeProvider {
                created: Mutex::new(Vec::new()),
                cleaned: Mutex::new(Vec::new()),
            })
            .expect("registered provider");
        assert!(registry.permits("fake"));
        assert!(registry.get("other").is_none());

        let credentials = Dns01Credentials::new(b"provider-secret".to_vec()).expect("credentials");
        let operation = Dns01Operation::new(Duration::from_secs(1)).expect("operation");
        let record = provider
            .create_txt_record(&challenge(), &credentials, &operation)
            .expect("create");
        assert!(record.matches(&challenge(), "fake"));
        provider
            .cleanup_txt_record(&record, &credentials, &operation)
            .expect("cleanup");
    }

    #[test]
    fn timeout_and_cancellation_are_bounded_and_categorical() {
        assert!(matches!(
            Dns01Operation::new(Duration::ZERO),
            Err(Dns01ProviderError::InvalidOperationTimeout)
        ));
        let cancellation = Dns01Cancellation::new();
        let operation =
            Dns01Operation::with_cancellation(Duration::from_secs(1), cancellation.clone())
                .expect("operation");
        cancellation.cancel();
        assert_eq!(operation.check(), Err(Dns01ProviderError::Cancelled));
    }

    #[test]
    fn credentials_challenges_and_records_never_debug_secret_values() {
        let credentials = Dns01Credentials::new(b"provider-secret".to_vec()).expect("credentials");
        let challenge = challenge();
        let record = Dns01Record::new(
            "fake",
            challenge.challenge_url(),
            challenge.record_name(),
            challenge.record_value().as_bytes().to_vec(),
            "record-1",
        )
        .expect("record");
        for debug in [
            format!("{credentials:?}"),
            format!("{challenge:?}"),
            format!("{record:?}"),
        ] {
            assert!(debug.contains("REDACTED"));
            assert!(!debug.contains("provider-secret"));
            assert!(!debug.contains("txt-value"));
        }
    }

    #[test]
    fn unallowlisted_and_dynamic_provider_names_fail_closed() {
        let mut registry = Dns01ProviderRegistry::new(["fake".into()]).expect("registry");
        assert_eq!(registry.register(FakeProvider::default()), Ok(()));
        assert_eq!(
            registry.register(FakeProvider::default()),
            Err(Dns01ProviderError::DuplicateProvider)
        );
        assert!(matches!(
            Dns01ProviderRegistry::new(["dynamic:provider".into()]),
            Err(Dns01ProviderError::InvalidProviderName)
        ));
    }
}
