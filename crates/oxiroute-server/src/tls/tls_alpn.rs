use std::{
    cmp::Ordering,
    collections::BTreeMap,
    fmt,
    net::IpAddr,
    sync::{
        Arc, RwLock,
        atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use openssl::{
    asn1::{Asn1Object, Asn1OctetString, Asn1Time},
    bn::{BigNum, MsbOption},
    ec::{EcGroup, EcKey},
    hash::MessageDigest,
    nid::Nid,
    pkey::{PKey, Private},
    x509::{
        X509, X509NameBuilder,
        extension::{BasicConstraints, ExtendedKeyUsage, KeyUsage, SubjectAlternativeName},
    },
};
use x509_parser::parse_x509_certificate;

pub const TLS_ALPN_PROTOCOL: &[u8] = b"acme-tls/1";
pub const TLS_ALPN_IDENTIFIER_OID: &str = "1.3.6.1.5.5.7.1.31";
pub const MAX_TLS_ALPN_CHALLENGES: usize = 1_024;

const MAX_TLS_ALPN_CHALLENGE_TTL_SECONDS: u64 = 3_600;
const MAX_KEY_AUTHORIZATION_BYTES: usize = 512;
const MAX_OPAQUE_ID_BYTES: usize = 256;
static NEXT_LEASE_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, thiserror::Error)]
pub enum TlsAlpnChallengeError {
    #[error("TLS-ALPN-01 identifier is invalid")]
    InvalidIdentifier,
    #[error("TLS-ALPN-01 key authorization is invalid")]
    InvalidKeyAuthorization,
    #[error("TLS-ALPN-01 challenge ownership metadata is invalid")]
    InvalidOwnership,
    #[error("TLS-ALPN-01 challenge lifetime is invalid")]
    InvalidLifetime,
    #[error("TLS-ALPN-01 certificate generation failed")]
    CertificateGeneration(#[source] openssl::error::ErrorStack),
    #[error("TLS-ALPN-01 certificate validation failed")]
    CertificateValidation(#[source] openssl::error::ErrorStack),
    #[error("TLS-ALPN-01 certificate does not match its challenge")]
    CertificateMismatch,
    #[error("TLS-ALPN-01 challenge record is invalid")]
    InvalidRecord,
    #[error("TLS-ALPN-01 challenge name is already provisioned")]
    DuplicateIdentifier,
    #[error("TLS-ALPN-01 challenge store capacity is exhausted")]
    CapacityExceeded,
}

#[derive(Clone)]
pub struct TlsAlpnChallenge {
    identity: Arc<TlsAlpnChallengeIdentity>,
}

impl fmt::Debug for TlsAlpnChallenge {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.identity.fmt(formatter)
    }
}

impl TlsAlpnChallenge {
    /// Generates the short-lived self-signed certificate required by RFC 8737.
    ///
    /// The certificate contains exactly one DNS SAN and one critical
    /// `id-pe-acmeIdentifier` extension containing the SHA-256 digest of the key authorization.
    /// Private material remains in memory and is never serialized by this type.
    ///
    /// # Errors
    ///
    /// Returns an error when challenge metadata is invalid or the maintained OpenSSL library
    /// cannot generate and validate the challenge identity.
    #[allow(clippy::too_many_arguments)]
    pub fn generate(
        identifier: &str,
        key_authorization: &str,
        account_id: &str,
        order_id: &str,
        authorization_id: &str,
        challenge_id: &str,
        created_at_unix_seconds: u64,
        expires_at_unix_seconds: u64,
    ) -> Result<Self, TlsAlpnChallengeError> {
        let identifier = normalize_identifier(identifier)?;
        if key_authorization.is_empty()
            || key_authorization.len() > MAX_KEY_AUTHORIZATION_BYTES
            || !key_authorization.is_ascii()
            || key_authorization.bytes().any(|byte| byte <= b' ')
        {
            return Err(TlsAlpnChallengeError::InvalidKeyAuthorization);
        }
        for value in [account_id, order_id, authorization_id, challenge_id] {
            if !valid_opaque_id(value) {
                return Err(TlsAlpnChallengeError::InvalidOwnership);
            }
        }
        if expires_at_unix_seconds <= created_at_unix_seconds
            || expires_at_unix_seconds.saturating_sub(created_at_unix_seconds)
                > MAX_TLS_ALPN_CHALLENGE_TTL_SECONDS
        {
            return Err(TlsAlpnChallengeError::InvalidLifetime);
        }

        let digest = openssl::sha::sha256(key_authorization.as_bytes());
        let private_key = generate_key().map_err(TlsAlpnChallengeError::CertificateGeneration)?;
        let certificate = generate_certificate(&identifier, &private_key, &digest)
            .map_err(TlsAlpnChallengeError::CertificateGeneration)?;
        validate_certificate(&certificate, &private_key, &identifier, &digest).map_err(
            |error| match error {
                TlsAlpnChallengeError::CertificateValidation(_) => error,
                other => other,
            },
        )?;

        Ok(Self {
            identity: Arc::new(TlsAlpnChallengeIdentity {
                identifier,
                account_id: account_id.into(),
                order_id: order_id.into(),
                authorization_id: authorization_id.into(),
                challenge_id: challenge_id.into(),
                created_at_unix_seconds,
                expires_at_unix_seconds,
                certificate,
                private_key,
                active: AtomicBool::new(true),
            }),
        })
    }
}

pub struct TlsAlpnChallengeIdentity {
    identifier: String,
    account_id: String,
    order_id: String,
    authorization_id: String,
    challenge_id: String,
    created_at_unix_seconds: u64,
    expires_at_unix_seconds: u64,
    certificate: X509,
    private_key: PKey<Private>,
    active: AtomicBool,
}

impl fmt::Debug for TlsAlpnChallengeIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TlsAlpnChallengeIdentity")
            .field("identifier", &self.identifier)
            .field("account_id", &self.account_id)
            .field("order_id", &self.order_id)
            .field("authorization_id", &self.authorization_id)
            .field("challenge_id", &self.challenge_id)
            .field("created_at_unix_seconds", &self.created_at_unix_seconds)
            .field("expires_at_unix_seconds", &self.expires_at_unix_seconds)
            .field("certificate", &"REDACTED")
            .field("private_key", &"REDACTED")
            .field("active", &self.active.load(AtomicOrdering::Acquire))
            .finish()
    }
}

impl TlsAlpnChallengeIdentity {
    #[must_use]
    pub fn identifier(&self) -> &str {
        &self.identifier
    }

    #[must_use]
    pub fn certificate(&self) -> &X509 {
        &self.certificate
    }

    #[must_use]
    pub fn private_key(&self) -> &PKey<Private> {
        &self.private_key
    }

    #[must_use]
    pub fn expires_at_unix_seconds(&self) -> u64 {
        self.expires_at_unix_seconds
    }

    #[must_use]
    pub(crate) fn usable(&self, now_unix_seconds: u64) -> bool {
        self.active.load(AtomicOrdering::Acquire) && now_unix_seconds < self.expires_at_unix_seconds
    }

    fn deactivate(&self) {
        self.active.store(false, AtomicOrdering::Release);
    }
}

struct StoredChallenge {
    identity: Arc<TlsAlpnChallengeIdentity>,
    owner_id: u64,
}

#[derive(Clone)]
pub struct TlsAlpnChallengeStore {
    inner: Arc<RwLock<BTreeMap<String, StoredChallenge>>>,
    capacity: usize,
}

impl fmt::Debug for TlsAlpnChallengeStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let count = self
            .inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len();
        formatter
            .debug_struct("TlsAlpnChallengeStore")
            .field("count", &count)
            .field("capacity", &self.capacity)
            .finish()
    }
}

impl Default for TlsAlpnChallengeStore {
    fn default() -> Self {
        Self::new(MAX_TLS_ALPN_CHALLENGES)
    }
}

impl TlsAlpnChallengeStore {
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(RwLock::new(BTreeMap::new())),
            capacity: capacity.min(MAX_TLS_ALPN_CHALLENGES),
        }
    }

    /// Publishes one exact-name challenge and returns its ownership-safe cleanup lease.
    ///
    /// # Errors
    ///
    /// Returns an error when the exact name is already active or the bounded store is full.
    pub fn provision(
        &self,
        challenge: TlsAlpnChallenge,
    ) -> Result<TlsAlpnChallengeLease, TlsAlpnChallengeError> {
        let TlsAlpnChallenge { identity } = challenge;
        if !identity.active.load(AtomicOrdering::Acquire) {
            return Err(TlsAlpnChallengeError::InvalidRecord);
        }
        let mut entries = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        entries.retain(|_, stored| {
            if stored.identity.expires_at_unix_seconds <= identity.created_at_unix_seconds {
                stored.identity.deactivate();
                false
            } else {
                true
            }
        });
        if entries.contains_key(identity.identifier()) {
            return Err(TlsAlpnChallengeError::DuplicateIdentifier);
        }
        if entries.len() >= self.capacity {
            return Err(TlsAlpnChallengeError::CapacityExceeded);
        }
        let owner_id = NEXT_LEASE_ID.fetch_add(1, AtomicOrdering::Relaxed);
        let identifier = identity.identifier.clone();
        entries.insert(
            identifier.clone(),
            StoredChallenge {
                identity: Arc::clone(&identity),
                owner_id,
            },
        );
        Ok(TlsAlpnChallengeLease {
            store: self.clone(),
            identifier: Some(identifier),
            owner_id,
            identity,
        })
    }

    /// Returns the active exact-name identity, removing expired or cancelled records first.
    #[must_use]
    pub fn lookup(
        &self,
        identifier: &str,
        now_unix_seconds: u64,
    ) -> Option<Arc<TlsAlpnChallengeIdentity>> {
        let identifier = normalize_identifier(identifier).ok()?;
        let mut entries = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let expired = entries
            .get(&identifier)
            .is_some_and(|stored| !stored.identity.usable(now_unix_seconds));
        if expired {
            if let Some(stored) = entries.remove(&identifier) {
                stored.identity.deactivate();
            }
            return None;
        }
        entries
            .get(&identifier)
            .map(|stored| Arc::clone(&stored.identity))
    }

    /// Removes all challenge identities owned by one opaque authorization.
    #[must_use]
    pub fn cancel_authorization(&self, authorization_id: &str) -> usize {
        self.cancel_matching(|identity| identity.authorization_id == authorization_id)
    }

    /// Removes all challenge identities owned by one opaque order.
    #[must_use]
    pub fn cancel_order(&self, order_id: &str) -> usize {
        self.cancel_matching(|identity| identity.order_id == order_id)
    }

    /// Removes expired or cancelled challenge identities.
    #[must_use]
    pub fn reap_expired(&self, now_unix_seconds: u64) -> usize {
        self.cancel_matching(|identity| !identity.usable(now_unix_seconds))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn cancel_matching(&self, predicate: impl Fn(&TlsAlpnChallengeIdentity) -> bool) -> usize {
        let mut entries = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut removed = 0;
        entries.retain(|_, stored| {
            if predicate(&stored.identity) {
                stored.identity.deactivate();
                removed += 1;
                false
            } else {
                true
            }
        });
        removed
    }

    fn cancel_owned(&self, identifier: &str, owner_id: u64) -> bool {
        let mut entries = self
            .inner
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if entries
            .get(identifier)
            .is_some_and(|stored| stored.owner_id == owner_id)
        {
            if let Some(stored) = entries.remove(identifier) {
                stored.identity.deactivate();
            }
            true
        } else {
            false
        }
    }
}

pub struct TlsAlpnChallengeLease {
    store: TlsAlpnChallengeStore,
    identifier: Option<String>,
    owner_id: u64,
    identity: Arc<TlsAlpnChallengeIdentity>,
}

impl fmt::Debug for TlsAlpnChallengeLease {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TlsAlpnChallengeLease")
            .field("active", &self.identifier.is_some())
            .finish_non_exhaustive()
    }
}

impl TlsAlpnChallengeLease {
    #[must_use]
    pub fn identity(&self) -> &Arc<TlsAlpnChallengeIdentity> {
        &self.identity
    }

    /// Completes the challenge and removes its temporary identity.
    pub fn complete(mut self) {
        self.cancel_inner();
    }

    /// Cancels the challenge and removes its temporary identity.
    pub fn cancel(mut self) {
        self.cancel_inner();
    }

    fn cancel_inner(&mut self) {
        if let Some(identifier) = self.identifier.take() {
            self.store.cancel_owned(&identifier, self.owner_id);
        }
    }
}

impl Drop for TlsAlpnChallengeLease {
    fn drop(&mut self) {
        self.cancel_inner();
    }
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn generate_key() -> Result<PKey<Private>, openssl::error::ErrorStack> {
    let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1)?;
    PKey::from_ec_key(EcKey::generate(&group)?)
}

fn generate_certificate(
    identifier: &str,
    private_key: &PKey<Private>,
    digest: &[u8; 32],
) -> Result<X509, openssl::error::ErrorStack> {
    let mut subject = X509NameBuilder::new()?;
    subject.append_entry_by_text("commonName", identifier)?;
    let subject = subject.build();

    let mut serial = BigNum::new()?;
    serial.rand(128, MsbOption::ONE, false)?;
    let serial = serial.to_asn1_integer()?;

    let mut builder = X509::builder()?;
    builder.set_version(2)?;
    builder.set_serial_number(&serial)?;
    builder.set_subject_name(&subject)?;
    builder.set_issuer_name(&subject)?;
    builder.set_pubkey(private_key)?;
    builder.set_not_before(Asn1Time::days_from_now(0)?.as_ref())?;
    builder.set_not_after(Asn1Time::days_from_now(1)?.as_ref())?;

    let context = builder.x509v3_context(None, None);
    builder.append_extension(
        SubjectAlternativeName::new()
            .dns(identifier)
            .build(&context)?,
    )?;
    builder.append_extension(BasicConstraints::new().critical().build()?)?;
    builder.append_extension(KeyUsage::new().critical().digital_signature().build()?)?;
    builder.append_extension(ExtendedKeyUsage::new().server_auth().build()?)?;

    let mut inner = Vec::with_capacity(2 + digest.len());
    inner.extend_from_slice(&[0x04, 0x20]);
    inner.extend_from_slice(digest);
    let extension_value = Asn1OctetString::new_from_bytes(&inner)?;
    let oid = Asn1Object::from_str(TLS_ALPN_IDENTIFIER_OID)?;
    builder.append_extension(openssl::x509::X509Extension::new_from_der(
        &oid,
        true,
        &extension_value,
    )?)?;
    builder.sign(private_key, MessageDigest::sha256())?;
    Ok(builder.build())
}

fn validate_certificate(
    certificate: &X509,
    private_key: &PKey<Private>,
    identifier: &str,
    digest: &[u8; 32],
) -> Result<(), TlsAlpnChallengeError> {
    let public_key = certificate
        .public_key()
        .map_err(TlsAlpnChallengeError::CertificateValidation)?;
    if !public_key.public_eq(private_key)
        || !certificate
            .verify(&public_key)
            .map_err(TlsAlpnChallengeError::CertificateValidation)?
        || certificate
            .subject_name()
            .to_der()
            .map_err(TlsAlpnChallengeError::CertificateValidation)?
            != certificate
                .issuer_name()
                .to_der()
                .map_err(TlsAlpnChallengeError::CertificateValidation)?
    {
        return Err(TlsAlpnChallengeError::CertificateMismatch);
    }
    let now = Asn1Time::days_from_now(0).map_err(TlsAlpnChallengeError::CertificateValidation)?;
    if certificate
        .not_before()
        .compare(&now)
        .map_err(TlsAlpnChallengeError::CertificateValidation)?
        == Ordering::Greater
        || certificate
            .not_after()
            .compare(&now)
            .map_err(TlsAlpnChallengeError::CertificateValidation)?
            != Ordering::Greater
    {
        return Err(TlsAlpnChallengeError::CertificateMismatch);
    }
    let sans = certificate
        .subject_alt_names()
        .ok_or(TlsAlpnChallengeError::CertificateMismatch)?;
    let names = sans
        .iter()
        .map(|name| {
            name.dnsname()
                .ok_or(TlsAlpnChallengeError::CertificateMismatch)
        })
        .collect::<Result<Vec<_>, _>>()?;
    if names != [identifier] {
        return Err(TlsAlpnChallengeError::CertificateMismatch);
    }

    let der = certificate
        .to_der()
        .map_err(TlsAlpnChallengeError::CertificateValidation)?;
    let (_, parsed) =
        parse_x509_certificate(&der).map_err(|_| TlsAlpnChallengeError::CertificateMismatch)?;
    let mut expected_value = Vec::with_capacity(2 + digest.len());
    expected_value.extend_from_slice(&[0x04, 0x20]);
    expected_value.extend_from_slice(digest);
    let extensions = parsed
        .extensions()
        .iter()
        .filter(|extension| extension.oid.to_id_string() == TLS_ALPN_IDENTIFIER_OID)
        .collect::<Vec<_>>();
    if extensions.len() != 1
        || !extensions[0].critical
        || extensions[0].value != expected_value.as_slice()
    {
        return Err(TlsAlpnChallengeError::CertificateMismatch);
    }
    Ok(())
}

fn normalize_identifier(identifier: &str) -> Result<String, TlsAlpnChallengeError> {
    let mut identifier = identifier.to_owned();
    identifier.make_ascii_lowercase();
    if !valid_dns_name(&identifier) || identifier.parse::<IpAddr>().is_ok() {
        return Err(TlsAlpnChallengeError::InvalidIdentifier);
    }
    Ok(identifier)
}

fn valid_dns_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value.is_ascii()
        && !value.ends_with('.')
        && !value.starts_with("*.")
        && !value.contains('*')
        && value.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
                && label
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && label
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
}

fn valid_opaque_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_OPAQUE_ID_BYTES
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn challenge(
        identifier: &str,
        created_at_unix_seconds: u64,
        expires_at_unix_seconds: u64,
    ) -> TlsAlpnChallenge {
        TlsAlpnChallenge::generate(
            identifier,
            "token.thumbprint",
            "account-1",
            "order-1",
            "authorization-1",
            "challenge-1",
            created_at_unix_seconds,
            expires_at_unix_seconds,
        )
        .expect("TLS-ALPN-01 challenge")
    }

    #[test]
    fn generates_exact_rfc8737_certificate_and_redacts_private_material() {
        let challenge = challenge("WWW.EXAMPLE.TEST", 1, 100);
        let debug = format!("{challenge:?}");
        assert!(debug.contains("www.example.test"));
        assert!(!debug.contains("token.thumbprint"));
        assert!(debug.contains("REDACTED"));
        let certificate = challenge.identity.certificate();
        assert_eq!(certificate.subject_alt_names().unwrap().len(), 1);
        assert_eq!(
            certificate
                .subject_alt_names()
                .unwrap()
                .iter()
                .next()
                .unwrap()
                .dnsname(),
            Some("www.example.test")
        );
        let der = certificate.to_der().expect("challenge certificate DER");
        let (_, parsed) = parse_x509_certificate(&der).expect("parse challenge certificate");
        let extension = parsed
            .extensions()
            .iter()
            .find(|extension| extension.oid.to_id_string() == TLS_ALPN_IDENTIFIER_OID)
            .expect("ACME identifier extension");
        let mut expected_value = vec![0x04, 0x20];
        expected_value.extend_from_slice(&openssl::sha::sha256(b"token.thumbprint"));
        assert!(extension.critical);
        assert_eq!(extension.value, expected_value.as_slice());
    }

    #[test]
    fn store_fences_expiry_cancellation_and_concurrent_ownership() {
        let store = TlsAlpnChallengeStore::new(1);
        let first = store
            .provision(challenge("one.example.test", 1, 100))
            .unwrap();
        assert!(matches!(
            store.provision(challenge("one.example.test", 2, 100)),
            Err(TlsAlpnChallengeError::DuplicateIdentifier)
        ));
        assert!(store.lookup("one.example.test", 99).is_some());
        first.cancel();
        assert!(store.lookup("one.example.test", 2).is_none());

        let expired = store
            .provision(challenge("expired.example.test", 1, 2))
            .unwrap();
        assert!(store.lookup("expired.example.test", 2).is_none());
        drop(expired);
        assert!(store.is_empty());
    }

    #[test]
    fn rejects_wildcard_ip_and_invalid_lifetime_without_partial_state() {
        assert!(matches!(
            TlsAlpnChallenge::generate(
                "*.example.test",
                "token.thumbprint",
                "account-1",
                "order-1",
                "authorization-1",
                "challenge-1",
                1,
                2,
            ),
            Err(TlsAlpnChallengeError::InvalidIdentifier)
        ));
        assert!(matches!(
            TlsAlpnChallenge::generate(
                "127.0.0.1",
                "token.thumbprint",
                "account-1",
                "order-1",
                "authorization-1",
                "challenge-1",
                1,
                2,
            ),
            Err(TlsAlpnChallengeError::InvalidIdentifier)
        ));
        assert!(matches!(
            TlsAlpnChallenge::generate(
                "example.test",
                "token.thumbprint",
                "account-1",
                "order-1",
                "authorization-1",
                "challenge-1",
                2,
                1,
            ),
            Err(TlsAlpnChallengeError::InvalidLifetime)
        ));
    }
}
