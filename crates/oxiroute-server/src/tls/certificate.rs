use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashSet},
    fmt,
    net::IpAddr,
    path::Path,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering as AtomicOrdering},
    },
};

use super::{
    CertificateIdentitySan, CertificateIdentitySans, MAX_CA_CERTIFICATE_BYTES,
    MAX_CERTIFICATE_CHAIN_BYTES, MAX_CERTIFICATES_IN_CHAIN, MAX_CLIENT_CA_CERTIFICATES,
    MAX_DH_PARAMETERS_BYTES, MAX_PRIVATE_KEY_BYTES, TlsAlpnChallengeIdentity,
    TlsAlpnChallengeStore, TlsBuildError, certificate_identity_sans, certificate_is_ca_capable,
    pem_labels, read_bounded_stable,
};
use crate::encoding::lower_hex;
use arc_swap::ArcSwap;
use async_trait::async_trait;
use openssl::{
    asn1::Asn1Time,
    bn::{BigNum, MsbOption},
    dh::Dh,
    ec::{EcGroup, EcKey},
    error::ErrorStack,
    hash::MessageDigest,
    nid::Nid,
    pkey::{Id, PKey, Params, Private},
    rsa::Rsa,
    sha::sha256,
    ssl::{
        NameType, SslAcceptor, SslMethod, SslOptions, SslSessionCacheMode, SslVerifyMode,
        SslVersion,
    },
    stack::Stack,
    x509::{
        X509, X509NameBuilder, X509PurposeId, X509StoreContext,
        extension::{BasicConstraints, ExtendedKeyUsage, KeyUsage, SubjectAlternativeName},
        store::X509StoreBuilder,
        verify::X509VerifyFlags,
    },
};
use oxiroute_config::{
    AlpnProtocol, SelfSignedKeyType, TlsClientAuthMode, TlsPolicy, TlsProfile, TlsSessionCache,
    TlsVersion,
};
use pingora::{
    listeners::{ALPN, TlsAccept, tls::TlsSettings},
    protocols::tls::CustomALPN,
    protocols::tls::TlsRef,
    tls::ext::{ssl_add_chain_cert, ssl_use_certificate, ssl_use_private_key},
    tls::ssl::{AlpnError, Ssl as PingoraSsl},
};
use x509_parser::parse_x509_certificate;

const CERTIFICATE_FILE: &str = "certificate chain";
const PRIVATE_KEY_FILE: &str = "private key";
const DH_PARAMETERS_FILE: &str = "DH parameters";
const ESTIMATED_SESSION_BYTES: u64 = 256;
const MAX_DNS_NAMES: usize = 100;
const MAX_SELF_SIGNED_VALIDITY_DAYS: u32 = 30;
const MAX_CERTIFICATE_PUBLICATION_ATTEMPTS: usize = 4;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum CertificateIdentity {
    Dns(String),
    Ip(IpAddr),
}

impl CertificateIdentity {
    fn into_string(self) -> String {
        match self {
            Self::Dns(dns_name) => dns_name,
            Self::Ip(ip) => ip.to_string(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertificateValidity {
    pub not_before: String,
    pub not_after: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CertificateMetadata {
    pub name: String,
    pub fingerprint_sha256: String,
    pub revision: String,
    pub dns_names: Vec<String>,
    pub validity: CertificateValidity,
    pub intermediate_count: usize,
}

/// One immutable, parsed identity. The OpenSSL key and certificate objects are intentionally
/// private and Debug only exposes public certificate metadata.
pub struct CertificateGeneration {
    leaf: X509,
    intermediates: Box<[X509]>,
    private_key: PKey<Private>,
    metadata: CertificateMetadata,
}

impl CertificateGeneration {
    /// Parses and preflights a file-backed identity without retaining either source path or PEM
    /// buffer in the resulting generation.
    ///
    /// # Errors
    ///
    /// Returns an error if either stable bounded read fails or the identity is unusable.
    pub fn from_files(
        name: impl Into<String>,
        declared_dns_names: &[String],
        certificate_chain_path: &Path,
        private_key_path: &Path,
    ) -> Result<Self, TlsBuildError> {
        let name = name.into();
        let chain_pem = read_bounded_stable(
            &name,
            CERTIFICATE_FILE,
            certificate_chain_path,
            MAX_CERTIFICATE_CHAIN_BYTES,
            false,
        )?;
        let key_pem = read_bounded_stable(
            &name,
            PRIVATE_KEY_FILE,
            private_key_path,
            MAX_PRIVATE_KEY_BYTES,
            true,
        )?;
        Self::from_pem(
            name,
            declared_dns_names,
            certificate_chain_path,
            &chain_pem,
            private_key_path,
            &key_pem,
        )
    }

    #[allow(clippy::too_many_lines)]
    pub(super) fn self_signed_development(
        name: impl Into<String>,
        declared_dns_names: &[String],
        validity_days: u32,
        key_type: SelfSignedKeyType,
    ) -> Result<Self, TlsBuildError> {
        let name = name.into();
        if validity_days == 0 || validity_days > MAX_SELF_SIGNED_VALIDITY_DAYS {
            return Err(TlsBuildError::InvalidSelfSignedValidity { certificate: name });
        }
        let mut identities = normalize_declared_dns_names(&name, declared_dns_names)?;
        identities.sort_unstable();
        let private_key = generate_self_signed_key(&name, key_type)?;

        let mut subject = X509NameBuilder::new().map_err(|source| {
            TlsBuildError::SelfSignedCertificateGeneration {
                certificate: name.clone(),
                source,
            }
        })?;
        subject
            .append_entry_by_text("commonName", &identities[0].clone().into_string())
            .map_err(|source| TlsBuildError::SelfSignedCertificateGeneration {
                certificate: name.clone(),
                source,
            })?;
        let subject = subject.build();

        let mut serial =
            BigNum::new().map_err(|source| TlsBuildError::SelfSignedCertificateGeneration {
                certificate: name.clone(),
                source,
            })?;
        serial.rand(128, MsbOption::ONE, false).map_err(|source| {
            TlsBuildError::SelfSignedCertificateGeneration {
                certificate: name.clone(),
                source,
            }
        })?;
        let serial = serial.to_asn1_integer().map_err(|source| {
            TlsBuildError::SelfSignedCertificateGeneration {
                certificate: name.clone(),
                source,
            }
        })?;

        let mut builder =
            X509::builder().map_err(|source| TlsBuildError::SelfSignedCertificateGeneration {
                certificate: name.clone(),
                source,
            })?;
        builder.set_version(2).map_err(|source| {
            TlsBuildError::SelfSignedCertificateGeneration {
                certificate: name.clone(),
                source,
            }
        })?;
        builder.set_serial_number(&serial).map_err(|source| {
            TlsBuildError::SelfSignedCertificateGeneration {
                certificate: name.clone(),
                source,
            }
        })?;
        builder.set_subject_name(&subject).map_err(|source| {
            TlsBuildError::SelfSignedCertificateGeneration {
                certificate: name.clone(),
                source,
            }
        })?;
        builder.set_issuer_name(&subject).map_err(|source| {
            TlsBuildError::SelfSignedCertificateGeneration {
                certificate: name.clone(),
                source,
            }
        })?;
        builder.set_pubkey(&private_key).map_err(|source| {
            TlsBuildError::SelfSignedCertificateGeneration {
                certificate: name.clone(),
                source,
            }
        })?;
        let not_before = Asn1Time::days_from_now(0).map_err(|source| {
            TlsBuildError::SelfSignedCertificateGeneration {
                certificate: name.clone(),
                source,
            }
        })?;
        let not_after = Asn1Time::days_from_now(validity_days).map_err(|source| {
            TlsBuildError::SelfSignedCertificateGeneration {
                certificate: name.clone(),
                source,
            }
        })?;
        builder.set_not_before(&not_before).map_err(|source| {
            TlsBuildError::SelfSignedCertificateGeneration {
                certificate: name.clone(),
                source,
            }
        })?;
        builder.set_not_after(&not_after).map_err(|source| {
            TlsBuildError::SelfSignedCertificateGeneration {
                certificate: name.clone(),
                source,
            }
        })?;

        let san = {
            let mut san = SubjectAlternativeName::new();
            for identity in &identities {
                match identity {
                    CertificateIdentity::Dns(dns_name) => san.dns(dns_name),
                    CertificateIdentity::Ip(ip) => san.ip(&ip.to_string()),
                };
            }
            let context = builder.x509v3_context(None, None);
            san.build(&context).map_err(|source| {
                TlsBuildError::SelfSignedCertificateGeneration {
                    certificate: name.clone(),
                    source,
                }
            })?
        };
        builder.append_extension(san).map_err(|source| {
            TlsBuildError::SelfSignedCertificateGeneration {
                certificate: name.clone(),
                source,
            }
        })?;
        builder
            .append_extension(
                BasicConstraints::new()
                    .critical()
                    .build()
                    .map_err(|source| TlsBuildError::SelfSignedCertificateGeneration {
                        certificate: name.clone(),
                        source,
                    })?,
            )
            .map_err(|source| TlsBuildError::SelfSignedCertificateGeneration {
                certificate: name.clone(),
                source,
            })?;
        builder
            .append_extension(
                KeyUsage::new()
                    .critical()
                    .digital_signature()
                    .key_encipherment()
                    .build()
                    .map_err(|source| TlsBuildError::SelfSignedCertificateGeneration {
                        certificate: name.clone(),
                        source,
                    })?,
            )
            .map_err(|source| TlsBuildError::SelfSignedCertificateGeneration {
                certificate: name.clone(),
                source,
            })?;
        builder
            .append_extension(
                ExtendedKeyUsage::new()
                    .server_auth()
                    .build()
                    .map_err(|source| TlsBuildError::SelfSignedCertificateGeneration {
                        certificate: name.clone(),
                        source,
                    })?,
            )
            .map_err(|source| TlsBuildError::SelfSignedCertificateGeneration {
                certificate: name.clone(),
                source,
            })?;
        let leaf = {
            builder
                .sign(&private_key, MessageDigest::sha256())
                .map_err(|source| TlsBuildError::SelfSignedCertificateGeneration {
                    certificate: name.clone(),
                    source,
                })?;
            builder.build()
        };
        let public_key =
            leaf.public_key()
                .map_err(|source| TlsBuildError::SelfSignedCertificateGeneration {
                    certificate: name.clone(),
                    source,
                })?;
        if !leaf.verify(&public_key).map_err(|source| {
            TlsBuildError::SelfSignedCertificateGeneration {
                certificate: name.clone(),
                source,
            }
        })? {
            return Err(TlsBuildError::SelfSignedSignatureMismatch { certificate: name });
        }
        validate_private_key(&name, &private_key)?;
        validate_leaf_key_usage(&name, &leaf)?;
        validate_current(&name, &leaf)?;
        let dns_names = dns_names(&name, &leaf, declared_dns_names)?;
        preflight(&name, &leaf, &[], &private_key)?;
        let metadata = metadata(&name, &leaf, &[], dns_names)?;

        Ok(Self {
            leaf,
            intermediates: Box::new([]),
            private_key,
            metadata,
        })
    }

    pub(crate) fn from_pem(
        name: impl Into<String>,
        declared_dns_names: &[String],
        certificate_chain_path: &Path,
        chain_pem: &[u8],
        private_key_path: &Path,
        key_pem: &[u8],
    ) -> Result<Self, TlsBuildError> {
        let name = name.into();
        let chain_labels = pem_labels(&name, CERTIFICATE_FILE, certificate_chain_path, chain_pem)?;
        if chain_labels.iter().any(|label| *label != "CERTIFICATE") {
            return Err(TlsBuildError::InvalidPem {
                owner: name,
                kind: CERTIFICATE_FILE,
                path: certificate_chain_path.into(),
                detail: "certificate chain may contain only CERTIFICATE blocks",
            });
        }
        if chain_labels.len() > MAX_CERTIFICATES_IN_CHAIN {
            return Err(TlsBuildError::TooManyChainCertificates {
                certificate: name,
                count: chain_labels.len(),
            });
        }
        let mut certificates =
            X509::stack_from_pem(chain_pem).map_err(|source| TlsBuildError::CertificateParse {
                certificate: name.clone(),
                path: certificate_chain_path.into(),
                source,
            })?;
        if certificates.len() != chain_labels.len() {
            return Err(TlsBuildError::InvalidPem {
                owner: name,
                kind: CERTIFICATE_FILE,
                path: certificate_chain_path.into(),
                detail: "not every certificate PEM block could be parsed",
            });
        }

        let key_labels = pem_labels(&name, PRIVATE_KEY_FILE, private_key_path, key_pem)?;
        let encrypted = key_labels.iter().any(|label| label.contains("ENCRYPTED"))
            || std::str::from_utf8(key_pem)
                .is_ok_and(|pem| pem.lines().any(|line| line.contains("4,ENCRYPTED")));
        if encrypted {
            return Err(TlsBuildError::EncryptedPrivateKey {
                certificate: name,
                path: private_key_path.into(),
            });
        }
        let valid_key_label = matches!(
            key_labels.as_slice(),
            ["PRIVATE KEY" | "RSA PRIVATE KEY" | "EC PRIVATE KEY"]
        );
        if !valid_key_label {
            return Err(TlsBuildError::PrivateKeyCount {
                certificate: name,
                path: private_key_path.into(),
            });
        }
        let private_key = PKey::private_key_from_pem(key_pem).map_err(|source| {
            TlsBuildError::PrivateKeyParse {
                certificate: name.clone(),
                path: private_key_path.into(),
                source,
            }
        })?;
        let leaf = certificates.remove(0);
        let intermediates = certificates.into_boxed_slice();
        let public_key = leaf
            .public_key()
            .map_err(|source| TlsBuildError::LeafPublicKey {
                certificate: name.clone(),
                source,
            })?;
        if !public_key.public_eq(&private_key) {
            return Err(TlsBuildError::PrivateKeyMismatch {
                certificate: name.clone(),
            });
        }
        validate_private_key(&name, &private_key)?;
        validate_leaf_key_usage(&name, &leaf)?;

        let dns_names = dns_names(&name, &leaf, declared_dns_names)?;
        validate_chain(&name, &leaf, &intermediates)?;
        preflight(&name, &leaf, &intermediates, &private_key)?;
        let metadata = metadata(&name, &leaf, &intermediates, dns_names)?;

        Ok(Self {
            leaf,
            intermediates,
            private_key,
            metadata,
        })
    }

    #[must_use]
    pub const fn metadata(&self) -> &CertificateMetadata {
        &self.metadata
    }

    pub(crate) fn install(&self, ssl: &mut TlsRef) -> Result<(), ErrorStack> {
        // Select the OpenSSL key slot first, then keep the leaf absent until the full chain is ready.
        ssl_use_private_key(ssl, &self.private_key)?;
        for intermediate in &self.intermediates {
            ssl_add_chain_cert(ssl, intermediate)?;
        }
        ssl_use_certificate(ssl, &self.leaf)
    }

    pub(crate) fn quic_certified_key(&self) -> Result<rustls::sign::CertifiedKey, String> {
        let certificates = std::iter::once(&self.leaf)
            .chain(self.intermediates.iter())
            .map(|certificate| {
                certificate
                    .to_der()
                    .map(rustls::pki_types::CertificateDer::from)
                    .map_err(|error| format!("certificate DER encoding failed: {error}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let private_key = self
            .private_key
            .private_key_to_pkcs8()
            .map_err(|error| format!("private key DER encoding failed: {error}"))?;
        let private_key = rustls::pki_types::PrivateKeyDer::Pkcs8(
            rustls::pki_types::PrivatePkcs8KeyDer::from(private_key),
        );
        rustls::sign::CertifiedKey::from_der(
            certificates,
            private_key,
            &rustls::crypto::ring::default_provider(),
        )
        .map_err(|error| format!("QUIC certificate and key are unusable: {error}"))
    }
}

fn generate_self_signed_key(
    name: &str,
    key_type: SelfSignedKeyType,
) -> Result<PKey<Private>, TlsBuildError> {
    match key_type {
        SelfSignedKeyType::EcdsaP256 => {
            let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).map_err(|source| {
                TlsBuildError::SelfSignedKeyGeneration {
                    certificate: name.into(),
                    source,
                }
            })?;
            let key = EcKey::generate(&group).map_err(|source| {
                TlsBuildError::SelfSignedKeyGeneration {
                    certificate: name.into(),
                    source,
                }
            })?;
            PKey::from_ec_key(key).map_err(|source| TlsBuildError::SelfSignedKeyGeneration {
                certificate: name.into(),
                source,
            })
        }
        SelfSignedKeyType::Rsa2048 => {
            let key =
                Rsa::generate(2048).map_err(|source| TlsBuildError::SelfSignedKeyGeneration {
                    certificate: name.into(),
                    source,
                })?;
            PKey::from_rsa(key).map_err(|source| TlsBuildError::SelfSignedKeyGeneration {
                certificate: name.into(),
                source,
            })
        }
    }
}

fn validate_private_key(name: &str, private_key: &PKey<Private>) -> Result<(), TlsBuildError> {
    let minimum_bits = match private_key.id() {
        Id::RSA | Id::RSA_PSS => 2_048,
        Id::EC => 256,
        _ => {
            return Err(TlsBuildError::UnsupportedPrivateKeyAlgorithm {
                certificate: name.into(),
            });
        }
    };
    let bits = private_key.bits();
    if bits < minimum_bits {
        return Err(TlsBuildError::PrivateKeyTooWeak {
            certificate: name.into(),
            bits,
            minimum_bits,
        });
    }
    Ok(())
}

fn validate_leaf_key_usage(name: &str, leaf: &X509) -> Result<(), TlsBuildError> {
    let der = leaf
        .to_der()
        .map_err(|source| TlsBuildError::CertificateKeyUsageEncoding {
            certificate: name.into(),
            source,
        })?;
    let (_, parsed) =
        parse_x509_certificate(&der).map_err(|_| TlsBuildError::CertificateKeyUsageInspection {
            certificate: name.into(),
        })?;
    let key_usage =
        parsed
            .key_usage()
            .map_err(|_| TlsBuildError::CertificateKeyUsageInspection {
                certificate: name.into(),
            })?;
    if key_usage.is_some_and(|usage| !usage.value.digital_signature()) {
        return Err(TlsBuildError::MissingDigitalSignatureKeyUsage {
            certificate: name.into(),
        });
    }
    Ok(())
}

impl fmt::Debug for CertificateGeneration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CertificateGeneration")
            .field("metadata", &self.metadata)
            .finish_non_exhaustive()
    }
}

pub struct ActiveCertificateGeneration {
    name: String,
    dns_names: Vec<String>,
    current: ArcSwap<CertificateGeneration>,
}

impl ActiveCertificateGeneration {
    #[must_use]
    pub fn new(initial: Arc<CertificateGeneration>) -> Self {
        Self {
            name: initial.metadata.name.clone(),
            dns_names: initial.metadata.dns_names.clone(),
            current: ArcSwap::from(initial),
        }
    }

    /// Returns one self-consistent generation snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Arc<CertificateGeneration> {
        self.current.load_full()
    }

    /// Publishes `replacement` only if `expected` is still the active Arc.
    ///
    /// # Errors
    ///
    /// Returns public revision metadata when another writer won the race.
    pub fn publish_if_current(
        &self,
        expected: &Arc<CertificateGeneration>,
        replacement: Arc<CertificateGeneration>,
    ) -> Result<(), CertificatePublishError> {
        if replacement.metadata.name != self.name {
            return Err(CertificatePublishError::IdentityMismatch {
                active_name: self.name.clone(),
                replacement_name: replacement.metadata.name.clone(),
            });
        }
        if replacement.metadata.dns_names != self.dns_names {
            return Err(CertificatePublishError::DnsNamesMismatch {
                identity: self.name.clone(),
                active_dns_names: self.dns_names.clone(),
                replacement_dns_names: replacement.metadata.dns_names.clone(),
            });
        }
        let previous = self.current.compare_and_swap(expected, replacement);
        if Arc::ptr_eq(&previous, expected) {
            Ok(())
        } else {
            Err(CertificatePublishError::GenerationChanged {
                expected_revision: expected.metadata.revision.clone(),
                active_revision: previous.metadata.revision.clone(),
            })
        }
    }

    pub(super) fn publish_prevalidated_if_current(
        &self,
        expected: &Arc<CertificateGeneration>,
        replacement: Arc<CertificateGeneration>,
    ) -> bool {
        debug_assert_eq!(replacement.metadata.name, self.name);
        debug_assert_eq!(replacement.metadata.dns_names, self.dns_names);
        Arc::ptr_eq(
            &self.current.compare_and_swap(expected, replacement),
            expected,
        )
    }

    pub(super) fn publish_transaction<T, E>(
        &self,
        gate: Option<&PublicationGate>,
        before_publish: &mut dyn FnMut(usize),
        mut load_candidate: impl FnMut() -> Result<(T, CertificateGeneration), E>,
    ) -> Result<CertificatePublication<T>, CertificatePublicationError<E>> {
        for attempt in 0..MAX_CERTIFICATE_PUBLICATION_ATTEMPTS {
            if gate.is_some_and(PublicationGate::is_stopped) {
                return Err(CertificatePublicationError::Stopped);
            }
            let (context, candidate) =
                load_candidate().map_err(CertificatePublicationError::Candidate)?;
            let expected = self.snapshot();
            let unchanged = candidate.metadata().revision == expected.metadata().revision;
            let replacement = if unchanged {
                Arc::clone(&expected)
            } else {
                Arc::new(candidate)
            };
            before_publish(attempt);
            let publish =
                || self.publish_prevalidated_if_current(&expected, Arc::clone(&replacement));
            let published = if let Some(gate) = gate {
                gate.publish(publish)
                    .ok_or(CertificatePublicationError::Stopped)?
            } else {
                publish()
            };
            if published {
                return Ok(if unchanged {
                    CertificatePublication::Unchanged(context)
                } else {
                    CertificatePublication::Activated(context)
                });
            }
        }

        Err(CertificatePublicationError::Conflict {
            attempts: MAX_CERTIFICATE_PUBLICATION_ATTEMPTS,
        })
    }
}

pub(super) enum CertificatePublication<T> {
    Unchanged(T),
    Activated(T),
}

pub(super) enum CertificatePublicationError<E> {
    Candidate(E),
    Conflict { attempts: usize },
    Stopped,
}

pub(crate) struct PublicationGate {
    stopped: AtomicBool,
    publication: Mutex<()>,
}

impl PublicationGate {
    pub(crate) fn new() -> Self {
        Self {
            stopped: AtomicBool::new(false),
            publication: Mutex::new(()),
        }
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.stopped.load(AtomicOrdering::Acquire)
    }

    pub(crate) fn stop(&self) {
        let _publication = self
            .publication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.stopped.store(true, AtomicOrdering::Release);
    }

    pub(crate) fn publish<T>(&self, publish: impl FnOnce() -> T) -> Option<T> {
        let _publication = self
            .publication
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.is_stopped() {
            None
        } else {
            Some(publish())
        }
    }
}

impl fmt::Debug for ActiveCertificateGeneration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ActiveCertificateGeneration")
            .field("metadata", self.snapshot().metadata())
            .finish()
    }
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum CertificatePublishError {
    #[error(
        "certificate identity `{replacement_name}` cannot replace active identity `{active_name}`"
    )]
    IdentityMismatch {
        active_name: String,
        replacement_name: String,
    },
    #[error("certificate identity `{identity}` cannot change its DNS subject alternative names")]
    DnsNamesMismatch {
        identity: String,
        active_dns_names: Vec<String>,
        replacement_dns_names: Vec<String>,
    },
    #[error(
        "certificate generation changed before publication (expected {expected_revision}, active {active_revision})"
    )]
    GenerationChanged {
        expected_revision: String,
        active_revision: String,
    },
}

struct ClientAuthPlan {
    mode: TlsClientAuthMode,
    ca_certificates: Arc<[X509]>,
    allowed_dns_names: Arc<[CertificateIdentity]>,
    h3_client_verifier: Option<Arc<dyn rustls::server::danger::ClientCertVerifier>>,
}

impl ClientAuthPlan {
    fn from_config(profile: &TlsProfile) -> Result<Self, TlsBuildError> {
        let client_auth = &profile.policy.client_auth;
        let allowed_dns_names = client_auth
            .allowed_dns_names
            .iter()
            .map(|dns_name| {
                client_identity(dns_name).ok_or_else(|| TlsBuildError::InvalidTlsClientAuthPolicy {
                    profile: profile.name.clone(),
                    detail: "allowed_dns_names must contain exact DNS names or IP addresses",
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

        let (ca_certificates, rustls_roots) = match client_auth.mode {
            TlsClientAuthMode::Disabled => {
                if client_auth.ca_certificate_path.is_some() || !allowed_dns_names.is_empty() {
                    return Err(TlsBuildError::InvalidTlsClientAuthPolicy {
                        profile: profile.name.clone(),
                        detail: "disabled client authentication cannot configure a CA or identities",
                    });
                }
                (
                    Arc::from(Vec::<X509>::new().into_boxed_slice()),
                    Arc::new(rustls::RootCertStore::empty()),
                )
            }
            TlsClientAuthMode::Optional | TlsClientAuthMode::Required => {
                let path = client_auth.ca_certificate_path.as_deref().ok_or_else(|| {
                    TlsBuildError::InvalidTlsClientAuthPolicy {
                        profile: profile.name.clone(),
                        detail: "enabled client authentication requires a CA certificate path",
                    }
                })?;
                load_client_ca_bundle(&profile.name, path)?
            }
        };

        let allowed_dns_names = Arc::from(allowed_dns_names.into_boxed_slice());
        let h3_client_verifier = match client_auth.mode {
            TlsClientAuthMode::Disabled => None,
            TlsClientAuthMode::Optional | TlsClientAuthMode::Required => {
                let builder = rustls::server::WebPkiClientVerifier::builder(rustls_roots);
                let delegate = match client_auth.mode {
                    TlsClientAuthMode::Optional => builder.allow_unauthenticated().build(),
                    TlsClientAuthMode::Required => builder.build(),
                    TlsClientAuthMode::Disabled => unreachable!(),
                }
                .map_err(|error| TlsBuildError::ClientCaRustlsVerifier {
                    profile: profile.name.clone(),
                    detail: error.to_string(),
                })?;
                Some(Arc::new(ExactClientCertificateVerifier {
                    delegate,
                    allowed_dns_names: Arc::clone(&allowed_dns_names),
                })
                    as Arc<dyn rustls::server::danger::ClientCertVerifier>)
            }
        };

        Ok(Self {
            mode: client_auth.mode,
            ca_certificates,
            allowed_dns_names,
            h3_client_verifier,
        })
    }

    fn apply(&self, profile: &str, settings: &mut TlsSettings) -> Result<(), TlsBuildError> {
        let mode = match self.mode {
            TlsClientAuthMode::Disabled => {
                settings.set_verify(SslVerifyMode::NONE);
                return Ok(());
            }
            TlsClientAuthMode::Optional => SslVerifyMode::PEER,
            TlsClientAuthMode::Required => {
                SslVerifyMode::PEER | SslVerifyMode::FAIL_IF_NO_PEER_CERT
            }
        };
        let mut store = X509StoreBuilder::new().map_err(|source| TlsBuildError::ClientCaStore {
            profile: profile.into(),
            source: Box::new(source),
        })?;
        store
            .set_flags(X509VerifyFlags::X509_STRICT | X509VerifyFlags::PARTIAL_CHAIN)
            .and_then(|()| store.set_purpose(X509PurposeId::SSL_CLIENT))
            .map_err(|source| TlsBuildError::ClientCaStore {
                profile: profile.into(),
                source: Box::new(source),
            })?;
        for certificate in self.ca_certificates.iter() {
            store
                .add_cert(certificate.clone())
                .map_err(|source| TlsBuildError::ClientCaStore {
                    profile: profile.into(),
                    source: Box::new(source),
                })?;
        }
        settings
            .set_verify_cert_store(store.build())
            .map_err(|source| TlsBuildError::ClientCaStore {
                profile: profile.into(),
                source: Box::new(source),
            })?;
        settings.set_verify_depth(
            u32::try_from(MAX_CERTIFICATES_IN_CHAIN).expect("certificate depth fits in u32"),
        );

        let mut ca_names = Stack::new().map_err(|source| TlsBuildError::ClientCaStore {
            profile: profile.into(),
            source: Box::new(source),
        })?;
        for certificate in self.ca_certificates.iter() {
            let name = certificate.subject_name().to_owned().map_err(|source| {
                TlsBuildError::ClientCaStore {
                    profile: profile.into(),
                    source: Box::new(source),
                }
            })?;
            ca_names
                .push(name)
                .map_err(|source| TlsBuildError::ClientCaStore {
                    profile: profile.into(),
                    source: Box::new(source),
                })?;
        }
        settings.set_client_ca_list(ca_names);
        let allowed_dns_names = Arc::clone(&self.allowed_dns_names);
        settings.set_verify_callback(mode, move |preverified, context| {
            let Some(certificate) = context.current_cert() else {
                return mode == SslVerifyMode::PEER;
            };
            preverified
                && (context.error_depth() != 0
                    || client_certificate_matches(certificate, &allowed_dns_names))
        });
        Ok(())
    }

    fn mode(&self) -> TlsClientAuthMode {
        self.mode
    }

    fn ca_configured(&self) -> bool {
        !self.ca_certificates.is_empty()
    }

    fn allowed_dns_name_count(&self) -> usize {
        self.allowed_dns_names.len()
    }

    fn h3_client_verifier(&self) -> Option<Arc<dyn rustls::server::danger::ClientCertVerifier>> {
        self.h3_client_verifier.clone()
    }
}

#[derive(Debug)]
struct ExactClientCertificateVerifier {
    delegate: Arc<dyn rustls::server::danger::ClientCertVerifier>,
    allowed_dns_names: Arc<[CertificateIdentity]>,
}

impl rustls::server::danger::ClientCertVerifier for ExactClientCertificateVerifier {
    fn offer_client_auth(&self) -> bool {
        self.delegate.offer_client_auth()
    }

    fn client_auth_mandatory(&self) -> bool {
        self.delegate.client_auth_mandatory()
    }

    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        self.delegate.root_hint_subjects()
    }

    fn verify_client_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        intermediates: &[rustls::pki_types::CertificateDer<'_>],
        now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
        let verified = self
            .delegate
            .verify_client_cert(end_entity, intermediates, now)?;
        let certificate = X509::from_der(end_entity.as_ref()).map_err(|_| {
            rustls::Error::InvalidCertificate(rustls::CertificateError::BadEncoding)
        })?;
        if !client_certificate_matches(&certificate, &self.allowed_dns_names) {
            return Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::NotValidForName,
            ));
        }
        Ok(verified)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.delegate.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        self.delegate.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.delegate.supported_verify_schemes()
    }
}

pub struct TlsProfilePlan {
    name: String,
    min_version: TlsVersion,
    alpn: ALPN,
    policy: TlsPolicy,
    dh_parameters: Option<Dh<Params>>,
    client_auth: ClientAuthPlan,
    tls_alpn_challenge_store: TlsAlpnChallengeStore,
    selector: Arc<CertificateSelector>,
}

impl TlsProfilePlan {
    pub(crate) fn from_config(
        profile: &TlsProfile,
        active_generations: BTreeMap<String, Arc<ActiveCertificateGeneration>>,
        tls_alpn_challenge_store: TlsAlpnChallengeStore,
    ) -> Result<Self, TlsBuildError> {
        let alpn = compile_alpn(&profile.name, &profile.alpn)?;
        let dh_parameters = profile
            .policy
            .dh_parameters_path
            .as_ref()
            .map(|path| {
                let pem = read_bounded_stable(
                    &profile.name,
                    DH_PARAMETERS_FILE,
                    path,
                    MAX_DH_PARAMETERS_BYTES,
                    false,
                )?;
                Dh::params_from_pem(&pem).map_err(|source| TlsBuildError::TlsDhParameters {
                    profile: profile.name.clone(),
                    path: path.clone(),
                    source,
                })
            })
            .transpose()?;
        let client_auth = ClientAuthPlan::from_config(profile)?;
        let selector = Arc::new(CertificateSelector::new(profile, active_generations)?);
        Ok(Self {
            name: profile.name.clone(),
            min_version: profile.min_version,
            alpn,
            policy: profile.policy.clone(),
            dh_parameters,
            client_auth,
            tls_alpn_challenge_store,
            selector,
        })
    }

    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    #[must_use]
    pub const fn min_version(&self) -> TlsVersion {
        self.min_version
    }

    #[must_use]
    pub const fn alpn(&self) -> &ALPN {
        &self.alpn
    }

    #[must_use]
    pub const fn policy(&self) -> &TlsPolicy {
        &self.policy
    }

    #[must_use]
    pub fn client_auth_mode(&self) -> TlsClientAuthMode {
        self.client_auth.mode()
    }

    #[must_use]
    pub fn client_auth_ca_configured(&self) -> bool {
        self.client_auth.ca_configured()
    }

    #[must_use]
    pub fn client_auth_allowed_dns_name_count(&self) -> usize {
        self.client_auth.allowed_dns_name_count()
    }

    pub(crate) fn h3_client_cert_verifier(
        &self,
    ) -> Option<Arc<dyn rustls::server::danger::ClientCertVerifier>> {
        self.client_auth.h3_client_verifier()
    }

    #[must_use]
    pub fn active_generation(
        &self,
        certificate: &str,
    ) -> Option<&Arc<ActiveCertificateGeneration>> {
        self.selector.active_generations.get(certificate)
    }

    #[must_use]
    pub fn default_certificate(&self) -> &str {
        &self.selector.default_certificate
    }

    #[must_use]
    pub fn selected_generation(&self, server_name: Option<&str>) -> Arc<CertificateGeneration> {
        self.selector.select(server_name).snapshot()
    }

    #[must_use]
    pub fn is_h2_only(&self) -> bool {
        matches!(self.alpn, ALPN::H2)
    }

    /// Constructs callback-backed Pingora settings without reading any source path.
    ///
    /// # Errors
    ///
    /// Returns an error if Pingora cannot create its acceptor or OpenSSL rejects the profile
    /// protocol settings.
    pub fn tls_settings(&self) -> Result<TlsSettings, TlsBuildError> {
        let callback = GenerationTlsAccept {
            profile: self.name.clone(),
            selector: Arc::clone(&self.selector),
        };
        let mut settings = TlsSettings::with_callbacks(Box::new(callback)).map_err(|source| {
            TlsBuildError::TlsSettings {
                profile: self.name.clone(),
                source,
            }
        })?;
        let min_version = match self.min_version {
            TlsVersion::Tls12 => SslVersion::TLS1_2,
            TlsVersion::Tls13 => SslVersion::TLS1_3,
        };
        settings
            .set_min_proto_version(Some(min_version))
            .map_err(|source| TlsBuildError::TlsProfileSettings {
                profile: self.name.clone(),
                source,
            })?;
        if let Some(cipher_list) = &self.policy.cipher_list {
            settings.set_cipher_list(cipher_list).map_err(|source| {
                TlsBuildError::TlsProfileSettings {
                    profile: self.name.clone(),
                    source,
                }
            })?;
        }
        if let Some(dh_parameters) = &self.dh_parameters {
            settings.set_tmp_dh(dh_parameters).map_err(|source| {
                TlsBuildError::TlsProfileSettings {
                    profile: self.name.clone(),
                    source,
                }
            })?;
        }
        if let Some(cache) = &self.policy.session_cache {
            let session_count =
                i32::try_from(cache.size_bytes / ESTIMATED_SESSION_BYTES).map_err(|_| {
                    TlsBuildError::InvalidTlsProfilePolicy {
                        profile: self.name.clone(),
                    }
                })?;
            settings
                .set_session_id_context(&session_id_context(&self.name, cache))
                .map_err(|source| TlsBuildError::TlsProfileSettings {
                    profile: self.name.clone(),
                    source,
                })?;
            settings.set_session_cache_size(session_count);
            settings.set_session_cache_mode(SslSessionCacheMode::SERVER);
        } else {
            settings.set_session_cache_mode(SslSessionCacheMode::OFF);
        }
        if let Some(timeout) = self.policy.session_timeout_seconds {
            let timeout =
                i32::try_from(timeout).map_err(|_| TlsBuildError::InvalidTlsProfilePolicy {
                    profile: self.name.clone(),
                })?;
            settings.set_session_timeout(timeout);
        }
        if self.policy.session_tickets {
            settings.clear_options(SslOptions::NO_TICKET);
        } else {
            settings.set_options(SslOptions::NO_TICKET);
        }
        if self.policy.prefer_server_ciphers {
            settings.set_options(SslOptions::CIPHER_SERVER_PREFERENCE);
        } else {
            settings.clear_options(SslOptions::CIPHER_SERVER_PREFERENCE);
        }
        let selection_index = tls_alpn_selection_index()?;
        let normal_alpn = self.alpn.clone();
        let challenge_store = self.tls_alpn_challenge_store.clone();
        settings.set_alpn_select_callback(move |ssl, offered| {
            select_alpn(
                ssl,
                offered,
                &normal_alpn,
                &challenge_store,
                selection_index,
            )
        });
        self.client_auth.apply(&self.name, &mut settings)?;
        Ok(settings)
    }
}

#[allow(clippy::missing_fields_in_debug)]
impl fmt::Debug for TlsProfilePlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TlsProfilePlan")
            .field("name", &self.name)
            .field("min_version", &self.min_version)
            .field("alpn", &self.alpn)
            .field("dh_parameters", &self.dh_parameters.is_some())
            .field("client_auth_mode", &self.client_auth.mode())
            .field(
                "client_auth_ca_configured",
                &self.client_auth.ca_configured(),
            )
            .field(
                "client_auth_allowed_dns_name_count",
                &self.client_auth.allowed_dns_name_count(),
            )
            .field("tls_alpn_challenge_store", &self.tls_alpn_challenge_store)
            .field("default_certificate", &self.selector.default_certificate)
            .field("certificates", &self.selector.active_generations.keys())
            .finish()
    }
}

fn client_identity(value: &str) -> Option<CertificateIdentity> {
    if let Ok(ip) = value.parse::<IpAddr>() {
        return Some(CertificateIdentity::Ip(canonical_ip(ip)));
    }
    let mut value = value.to_owned();
    value.make_ascii_lowercase();
    (valid_dns_name(&value) && !value.starts_with("*.")).then_some(CertificateIdentity::Dns(value))
}

fn load_client_ca_bundle(
    profile: &str,
    path: &Path,
) -> Result<(Arc<[X509]>, Arc<rustls::RootCertStore>), TlsBuildError> {
    const CLIENT_CA_FILE: &str = "client CA bundle";

    let pem = read_bounded_stable(
        profile,
        CLIENT_CA_FILE,
        path,
        MAX_CA_CERTIFICATE_BYTES,
        false,
    )?;
    let labels = pem_labels(profile, CLIENT_CA_FILE, path, &pem)?;
    if labels.iter().any(|label| *label != "CERTIFICATE") {
        return Err(TlsBuildError::InvalidPem {
            owner: profile.into(),
            kind: CLIENT_CA_FILE,
            path: path.into(),
            detail: "client CA bundle may contain only CERTIFICATE blocks",
        });
    }
    if labels.len() > MAX_CLIENT_CA_CERTIFICATES {
        return Err(TlsBuildError::TooManyClientCaCertificates {
            profile: profile.into(),
        });
    }
    let certificates =
        X509::stack_from_pem(&pem).map_err(|source| TlsBuildError::ClientCaParse {
            profile: profile.into(),
            path: path.into(),
            source,
        })?;
    if certificates.len() != labels.len() {
        return Err(TlsBuildError::InvalidPem {
            owner: profile.into(),
            kind: CLIENT_CA_FILE,
            path: path.into(),
            detail: "not every client CA certificate PEM block could be parsed",
        });
    }

    let mut unique_der = HashSet::with_capacity(certificates.len());
    let mut rustls_roots = rustls::RootCertStore::empty();
    for (index, certificate) in certificates.iter().enumerate() {
        validate_client_ca_current(profile, index, certificate)?;
        let ca_capable = certificate_is_ca_capable(certificate).map_err(|source| {
            TlsBuildError::ClientCaCertificateInspection {
                profile: profile.into(),
                index,
                source: Box::new(source),
            }
        })?;
        if !ca_capable {
            return Err(TlsBuildError::NonCaClientCertificate {
                profile: profile.into(),
                index,
            });
        }
        let der = certificate.to_der().map_err(|source| {
            TlsBuildError::ClientCaCertificateInspection {
                profile: profile.into(),
                index,
                source: Box::new(source),
            }
        })?;
        if !unique_der.insert(der.clone()) {
            return Err(TlsBuildError::DuplicateClientCaCertificate {
                profile: profile.into(),
                index,
            });
        }
        rustls_roots
            .add(rustls::pki_types::CertificateDer::from(der))
            .map_err(|error| TlsBuildError::ClientCaRustlsCertificate {
                profile: profile.into(),
                index,
                detail: error.to_string(),
            })?;
    }
    preflight_client_ca_store(profile, &certificates)?;
    Ok((
        Arc::from(certificates.into_boxed_slice()),
        Arc::new(rustls_roots),
    ))
}

fn validate_client_ca_current(
    profile: &str,
    index: usize,
    certificate: &X509,
) -> Result<(), TlsBuildError> {
    let now =
        Asn1Time::days_from_now(0).map_err(|_| TlsBuildError::ClientCaCertificateInvalid {
            profile: profile.into(),
            index,
            detail: "current validity could not be evaluated",
        })?;
    let not_before = certificate.not_before().compare(&now).map_err(|_| {
        TlsBuildError::ClientCaCertificateInvalid {
            profile: profile.into(),
            index,
            detail: "current validity could not be evaluated",
        }
    })?;
    if not_before == Ordering::Greater {
        return Err(TlsBuildError::ClientCaCertificateInvalid {
            profile: profile.into(),
            index,
            detail: "certificate is not yet valid",
        });
    }
    let not_after = certificate.not_after().compare(&now).map_err(|_| {
        TlsBuildError::ClientCaCertificateInvalid {
            profile: profile.into(),
            index,
            detail: "current validity could not be evaluated",
        }
    })?;
    if not_after != Ordering::Greater {
        return Err(TlsBuildError::ClientCaCertificateInvalid {
            profile: profile.into(),
            index,
            detail: "certificate is expired",
        });
    }
    Ok(())
}

fn preflight_client_ca_store(profile: &str, certificates: &[X509]) -> Result<(), TlsBuildError> {
    let mut store = X509StoreBuilder::new().map_err(|source| TlsBuildError::ClientCaStore {
        profile: profile.into(),
        source: Box::new(source),
    })?;
    store
        .set_flags(X509VerifyFlags::X509_STRICT | X509VerifyFlags::PARTIAL_CHAIN)
        .and_then(|()| store.set_purpose(X509PurposeId::SSL_CLIENT))
        .map_err(|source| TlsBuildError::ClientCaStore {
            profile: profile.into(),
            source: Box::new(source),
        })?;
    for certificate in certificates {
        store
            .add_cert(certificate.clone())
            .map_err(|source| TlsBuildError::ClientCaStore {
                profile: profile.into(),
                source: Box::new(source),
            })?;
    }
    let _store = store.build();
    Ok(())
}

fn client_certificate_matches(
    certificate: &openssl::x509::X509Ref,
    allowed_dns_names: &[CertificateIdentity],
) -> bool {
    let CertificateIdentitySans::Names(sans) = certificate_identity_sans(certificate)
        .ok()
        .unwrap_or(CertificateIdentitySans::Malformed)
    else {
        return false;
    };
    let identities = sans
        .into_iter()
        .filter_map(|san| match san {
            CertificateIdentitySan::Dns(value) => {
                let value = String::from_utf8(value).ok()?.to_ascii_lowercase();
                (valid_dns_name(&value) && !value.starts_with("*."))
                    .then_some(CertificateIdentity::Dns(value))
            }
            CertificateIdentitySan::Ip(ip) => Some(CertificateIdentity::Ip(canonical_ip(ip))),
        })
        .collect::<Vec<_>>();
    !identities.is_empty()
        && (allowed_dns_names.is_empty()
            || identities
                .iter()
                .any(|identity| allowed_dns_names.contains(identity)))
}

fn session_id_context(profile: &str, cache: &TlsSessionCache) -> [u8; 32] {
    sha256(format!("oxiroute-session-cache:{profile}:{}", cache.name).as_bytes())
}

struct CertificateSelector {
    default_certificate: String,
    active_generations: BTreeMap<String, Arc<ActiveCertificateGeneration>>,
    exact_names: BTreeMap<String, String>,
    wildcard_suffixes: BTreeMap<String, String>,
}

impl CertificateSelector {
    fn new(
        profile: &TlsProfile,
        active_generations: BTreeMap<String, Arc<ActiveCertificateGeneration>>,
    ) -> Result<Self, TlsBuildError> {
        if !active_generations.contains_key(&profile.default_certificate) {
            return Err(TlsBuildError::ProfileDefaultNotListed {
                profile: profile.name.clone(),
                certificate: profile.default_certificate.clone(),
            });
        }

        let mut exact_names = BTreeMap::new();
        let mut wildcard_suffixes = BTreeMap::new();
        for (certificate_name, active_generation) in &active_generations {
            for dns_name in &active_generation.snapshot().metadata().dns_names {
                if dns_name.parse::<IpAddr>().is_ok() {
                    continue;
                }
                let (names, name) = if let Some(suffix) = dns_name.strip_prefix("*.") {
                    (&mut wildcard_suffixes, suffix)
                } else {
                    (&mut exact_names, dns_name.as_str())
                };
                if let Some(first_certificate) = names.insert(name.into(), certificate_name.clone())
                {
                    return Err(TlsBuildError::OverlappingProfileDnsName {
                        profile: profile.name.clone(),
                        dns_name: dns_name.clone(),
                        first_certificate,
                        second_certificate: certificate_name.clone(),
                    });
                }
            }
        }

        Ok(Self {
            default_certificate: profile.default_certificate.clone(),
            active_generations,
            exact_names,
            wildcard_suffixes,
        })
    }

    fn select(&self, server_name: Option<&str>) -> &Arc<ActiveCertificateGeneration> {
        let certificate_name = server_name
            .and_then(normalize_server_name)
            .and_then(|name| {
                self.exact_names.get(&name).or_else(|| {
                    let (label, suffix) = name.split_once('.')?;
                    if label.is_empty() {
                        None
                    } else {
                        self.wildcard_suffixes.get(suffix)
                    }
                })
            })
            .unwrap_or(&self.default_certificate);
        &self.active_generations[certificate_name]
    }
}

fn normalize_server_name(server_name: &str) -> Option<String> {
    let mut server_name = server_name.to_owned();
    server_name.make_ascii_lowercase();
    (valid_dns_name(&server_name) && !server_name.starts_with("*.")).then_some(server_name)
}

struct GenerationTlsAccept {
    profile: String,
    selector: Arc<CertificateSelector>,
}

enum TlsAlpnSelection {
    Challenge(Arc<TlsAlpnChallengeIdentity>),
}

static TLS_ALPN_SELECTION_INDEX: OnceLock<
    Result<openssl::ex_data::Index<PingoraSsl, TlsAlpnSelection>, String>,
> = OnceLock::new();

fn tls_alpn_selection_index()
-> Result<openssl::ex_data::Index<PingoraSsl, TlsAlpnSelection>, TlsBuildError> {
    TLS_ALPN_SELECTION_INDEX
        .get_or_init(|| PingoraSsl::new_ex_index().map_err(|error| error.to_string()))
        .as_ref()
        .copied()
        .map_err(|detail| TlsBuildError::TlsAlpnSelectionIndex {
            detail: detail.clone(),
        })
}

#[async_trait]
impl TlsAccept for GenerationTlsAccept {
    async fn certificate_callback(&self, ssl: &mut TlsRef) {
        let selection_index = match tls_alpn_selection_index() {
            Ok(index) => index,
            Err(error) => {
                log::error!(
                    "failed to initialize TLS-ALPN-01 certificate selection for profile `{}`: {}",
                    self.profile,
                    error
                );
                return;
            }
        };
        let challenge = ssl
            .ex_data(selection_index)
            .map(|selection| match selection {
                TlsAlpnSelection::Challenge(challenge) => Arc::clone(challenge),
            });
        if let Some(challenge) = challenge {
            if !challenge.usable(super::tls_alpn::unix_now()) {
                log::warn!(
                    "TLS-ALPN-01 challenge identity expired or was cancelled for `{}`",
                    challenge.identifier()
                );
                return;
            }
            let result = ssl_use_private_key(ssl, challenge.private_key());
            let result = result.and_then(|()| ssl_use_certificate(ssl, challenge.certificate()));
            if let Err(error) = result {
                log::error!(
                    "failed to install TLS-ALPN-01 challenge identity for profile `{}`: {}",
                    self.profile,
                    error
                );
            }
            return;
        }

        let generation = self
            .selector
            .select(ssl.servername(NameType::HOST_NAME))
            .snapshot();
        if let Err(error) = generation.install(ssl) {
            log::error!(
                "failed to install certificate generation for TLS profile `{}` revision {}: {}",
                self.profile,
                generation.metadata.revision,
                error
            );
        }
    }
}

fn select_alpn<'a>(
    ssl: &mut TlsRef,
    offered: &'a [u8],
    normal_alpn: &ALPN,
    challenge_store: &TlsAlpnChallengeStore,
    selection_index: openssl::ex_data::Index<PingoraSsl, TlsAlpnSelection>,
) -> Result<&'a [u8], AlpnError> {
    let challenge_protocol = find_offered_protocol(offered, super::TLS_ALPN_PROTOCOL)?;
    if let Some(challenge_protocol) = challenge_protocol {
        let Some(server_name) = ssl.servername(NameType::HOST_NAME) else {
            return Err(AlpnError::ALERT_FATAL);
        };
        let Some(challenge) = challenge_store.lookup(server_name, super::tls_alpn::unix_now())
        else {
            return Err(AlpnError::ALERT_FATAL);
        };
        ssl.set_ex_data(selection_index, TlsAlpnSelection::Challenge(challenge));
        return Ok(challenge_protocol);
    }

    if !valid_alpn_wire(offered) {
        return Err(AlpnError::ALERT_FATAL);
    }
    match select_normal_alpn(normal_alpn, offered)? {
        Some(protocol) => Ok(protocol),
        None if matches!(normal_alpn, ALPN::H2) => Err(AlpnError::ALERT_FATAL),
        None => Err(AlpnError::NOACK),
    }
}

fn select_normal_alpn<'a>(alpn: &ALPN, offered: &'a [u8]) -> Result<Option<&'a [u8]>, AlpnError> {
    match alpn {
        ALPN::H1 => find_offered_protocol(offered, b"http/1.1"),
        ALPN::H2 => find_offered_protocol(offered, b"h2"),
        ALPN::H2H1 => find_offered_protocol(offered, b"h2")?.map_or_else(
            || find_offered_protocol(offered, b"http/1.1"),
            |protocol| Ok(Some(protocol)),
        ),
        ALPN::Custom(custom) => find_offered_protocol(offered, custom.protocol()),
    }
}

fn find_offered_protocol<'a>(
    offered: &'a [u8],
    wanted: &[u8],
) -> Result<Option<&'a [u8]>, AlpnError> {
    let mut cursor = 0;
    while cursor < offered.len() {
        let length = usize::from(offered[cursor]);
        cursor += 1;
        if length == 0 || cursor.saturating_add(length) > offered.len() {
            return Err(AlpnError::ALERT_FATAL);
        }
        let protocol = &offered[cursor..cursor + length];
        if protocol == wanted {
            return Ok(Some(protocol));
        }
        cursor += length;
    }
    Ok(None)
}

fn valid_alpn_wire(offered: &[u8]) -> bool {
    find_offered_protocol(offered, &[]).is_ok()
}

fn compile_alpn(profile: &str, protocols: &[AlpnProtocol]) -> Result<ALPN, TlsBuildError> {
    match protocols {
        [AlpnProtocol::Http11] => Ok(ALPN::H1),
        [AlpnProtocol::H2] => Ok(ALPN::H2),
        [AlpnProtocol::H2, AlpnProtocol::Http11] => Ok(ALPN::H2H1),
        [AlpnProtocol::H3] => Ok(ALPN::Custom(CustomALPN::new(b"h3".to_vec()))),
        _ => Err(TlsBuildError::InvalidProfileAlpn {
            profile: profile.into(),
        }),
    }
}

fn validate_current(name: &str, leaf: &X509) -> Result<(), TlsBuildError> {
    let now = Asn1Time::days_from_now(0).map_err(|source| TlsBuildError::CertificateValidity {
        certificate: name.into(),
        source,
    })?;
    let not_before =
        leaf.not_before()
            .compare(&now)
            .map_err(|source| TlsBuildError::CertificateValidity {
                certificate: name.into(),
                source,
            })?;
    if not_before == Ordering::Greater {
        return Err(TlsBuildError::CertificateNotYetValid {
            certificate: name.into(),
            not_before: leaf.not_before().to_string(),
        });
    }
    let not_after =
        leaf.not_after()
            .compare(&now)
            .map_err(|source| TlsBuildError::CertificateValidity {
                certificate: name.into(),
                source,
            })?;
    if not_after != Ordering::Greater {
        return Err(TlsBuildError::CertificateExpired {
            certificate: name.into(),
            not_after: leaf.not_after().to_string(),
        });
    }
    Ok(())
}

fn validate_chain(name: &str, leaf: &X509, intermediates: &[X509]) -> Result<(), TlsBuildError> {
    if intermediates.is_empty() {
        return Err(TlsBuildError::IncompleteCertificateChain {
            certificate: name.into(),
        });
    }

    validate_current(name, leaf)?;
    for (offset, certificate) in intermediates.iter().enumerate() {
        validate_chain_certificate_current(name, offset + 1, certificate)?;
    }

    let certificates = std::iter::once(leaf)
        .chain(intermediates)
        .collect::<Vec<_>>();
    for (subject_index, adjacent) in certificates.windows(2).enumerate() {
        let subject = adjacent[0];
        let issuer = adjacent[1];
        let issuer_index = subject_index + 1;
        let issuer_check = issuer.issued(subject);
        if issuer_check != openssl::x509::X509VerifyResult::OK {
            return Err(TlsBuildError::InvalidChainIssuer {
                certificate: name.into(),
                subject_index,
                issuer_index,
                reason: issuer_check,
            });
        }
        let issuer_key =
            issuer
                .public_key()
                .map_err(|source| TlsBuildError::ChainIssuerPublicKey {
                    certificate: name.into(),
                    issuer_index,
                    source: Box::new(source),
                })?;
        let signature_valid = subject.verify(&issuer_key).map_err(|source| {
            TlsBuildError::ChainSignatureValidation {
                certificate: name.into(),
                subject_index,
                source: Box::new(source),
            }
        })?;
        if !signature_valid {
            return Err(TlsBuildError::InvalidChainSignature {
                certificate: name.into(),
                subject_index,
                issuer_index,
            });
        }
    }

    for (offset, issuer) in intermediates.iter().enumerate() {
        let index = offset + 1;
        let ca_capable = certificate_is_ca_capable(issuer).map_err(|source| {
            TlsBuildError::ChainIssuerInspection {
                certificate: name.into(),
                index,
                source: Box::new(source),
            }
        })?;
        if !ca_capable {
            return Err(TlsBuildError::NonCaChainIssuer {
                certificate: name.into(),
                index,
            });
        }
    }

    verify_chain(name, leaf, intermediates)
}

fn validate_chain_certificate_current(
    name: &str,
    index: usize,
    certificate: &X509,
) -> Result<(), TlsBuildError> {
    let now = Asn1Time::days_from_now(0).map_err(|source| TlsBuildError::CertificateValidity {
        certificate: name.into(),
        source,
    })?;
    let not_before = certificate.not_before().compare(&now).map_err(|source| {
        TlsBuildError::CertificateValidity {
            certificate: name.into(),
            source,
        }
    })?;
    if not_before == Ordering::Greater {
        return Err(TlsBuildError::ChainCertificateNotYetValid {
            certificate: name.into(),
            index,
            not_before: certificate.not_before().to_string(),
        });
    }
    let not_after = certificate.not_after().compare(&now).map_err(|source| {
        TlsBuildError::CertificateValidity {
            certificate: name.into(),
            source,
        }
    })?;
    if not_after != Ordering::Greater {
        return Err(TlsBuildError::ChainCertificateExpired {
            certificate: name.into(),
            index,
            not_after: certificate.not_after().to_string(),
        });
    }
    Ok(())
}

fn verify_chain(name: &str, leaf: &X509, intermediates: &[X509]) -> Result<(), TlsBuildError> {
    let setup_error = |source| TlsBuildError::ChainVerificationSetup {
        certificate: name.into(),
        source: Box::new(source),
    };
    let mut store = X509StoreBuilder::new().map_err(setup_error)?;
    store
        .set_flags(X509VerifyFlags::X509_STRICT | X509VerifyFlags::PARTIAL_CHAIN)
        .map_err(setup_error)?;
    store
        .set_purpose(X509PurposeId::SSL_SERVER)
        .map_err(setup_error)?;
    store
        .add_cert(intermediates.last().expect("chain is nonempty").clone())
        .map_err(setup_error)?;
    let store = store.build();

    let mut untrusted = Stack::new().map_err(setup_error)?;
    for intermediate in &intermediates[..intermediates.len() - 1] {
        untrusted.push(intermediate.clone()).map_err(setup_error)?;
    }
    let mut context = X509StoreContext::new().map_err(setup_error)?;
    let (verified, reason, depth, verified_chain_length) = context
        .init(&store, leaf, &untrusted, |context| {
            let verified = context.verify_cert()?;
            Ok((
                verified,
                context.error(),
                context.error_depth(),
                context.chain().map_or(0, openssl::stack::StackRef::len),
            ))
        })
        .map_err(setup_error)?;
    if !verified {
        return Err(TlsBuildError::ChainVerification {
            certificate: name.into(),
            depth,
            reason,
        });
    }
    if verified_chain_length != intermediates.len() + 1 {
        return Err(TlsBuildError::IncompleteVerifiedChain {
            certificate: name.into(),
        });
    }
    Ok(())
}

fn dns_names(
    name: &str,
    leaf: &X509,
    declared_dns_names: &[String],
) -> Result<Vec<String>, TlsBuildError> {
    let mut declared = normalize_declared_dns_names(name, declared_dns_names)?;
    let raw_identities =
        certificate_identity_sans(leaf).map_err(|source| TlsBuildError::DnsSanInspection {
            certificate: name.into(),
            source: Box::new(source),
        })?;
    let raw_identities = match raw_identities {
        CertificateIdentitySans::Missing => {
            return Err(TlsBuildError::MissingDnsSan {
                certificate: name.into(),
            });
        }
        CertificateIdentitySans::Malformed => {
            return Err(TlsBuildError::InvalidDnsSanEncoding {
                certificate: name.into(),
            });
        }
        CertificateIdentitySans::Names(raw_identities) => raw_identities,
    };
    let actual = raw_identities
        .into_iter()
        .map(|identity| match identity {
            CertificateIdentitySan::Dns(dns_name) => {
                let mut dns_name = String::from_utf8(dns_name).map_err(|_| {
                    TlsBuildError::InvalidDnsSanEncoding {
                        certificate: name.into(),
                    }
                })?;
                dns_name.make_ascii_lowercase();
                if !valid_dns_name(&dns_name) {
                    return Err(TlsBuildError::InvalidDnsSan {
                        certificate: name.into(),
                        dns_name,
                    });
                }
                Ok(CertificateIdentity::Dns(dns_name))
            }
            CertificateIdentitySan::Ip(ip) => Ok(CertificateIdentity::Ip(canonical_ip(ip))),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if actual.is_empty() {
        return Err(TlsBuildError::MissingDnsSan {
            certificate: name.into(),
        });
    }
    let actual = actual.into_iter().collect::<HashSet<_>>();
    if !declared.iter().all(|identity| actual.contains(identity)) {
        return Err(TlsBuildError::DnsSanMismatch {
            certificate: name.into(),
        });
    }
    declared.sort_unstable();
    Ok(declared
        .into_iter()
        .map(CertificateIdentity::into_string)
        .collect())
}

fn normalize_declared_dns_names(
    name: &str,
    declared_dns_names: &[String],
) -> Result<Vec<CertificateIdentity>, TlsBuildError> {
    if declared_dns_names.is_empty() || declared_dns_names.len() > MAX_DNS_NAMES {
        return Err(TlsBuildError::InvalidDeclaredDnsNames {
            certificate: name.into(),
        });
    }
    let mut unique = HashSet::with_capacity(declared_dns_names.len());
    let mut normalized = Vec::with_capacity(declared_dns_names.len());
    for dns_name in declared_dns_names {
        if let Ok(ip) = dns_name.parse::<IpAddr>() {
            let identity = CertificateIdentity::Ip(canonical_ip(ip));
            if !unique.insert(identity.clone()) {
                return Err(TlsBuildError::InvalidDeclaredDnsNames {
                    certificate: name.into(),
                });
            }
            normalized.push(identity);
            continue;
        }
        let mut dns_name = dns_name.clone();
        dns_name.make_ascii_lowercase();
        if !valid_dns_name(&dns_name) {
            return Err(TlsBuildError::InvalidDeclaredDnsNames {
                certificate: name.into(),
            });
        }
        let identity = CertificateIdentity::Dns(dns_name);
        if !unique.insert(identity.clone()) {
            return Err(TlsBuildError::InvalidDeclaredDnsNames {
                certificate: name.into(),
            });
        }
        normalized.push(identity);
    }
    Ok(normalized)
}

fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ipv6) => ipv6.to_ipv4_mapped().map_or(IpAddr::V6(ipv6), IpAddr::V4),
        IpAddr::V4(_) => ip,
    }
}

fn valid_dns_name(dns_name: &str) -> bool {
    if !dns_name.is_ascii()
        || dns_name.is_empty()
        || dns_name.len() > 253
        || dns_name.ends_with('.')
    {
        return false;
    }
    let exact_name = if let Some(exact_name) = dns_name.strip_prefix("*.") {
        if exact_name.parse::<IpAddr>().is_ok() {
            return false;
        }
        exact_name
    } else {
        if dns_name.contains('*') || dns_name.parse::<IpAddr>().is_ok() {
            return false;
        }
        dns_name
    };
    !exact_name.is_empty()
        && exact_name.split('.').all(|label| {
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

fn preflight(
    name: &str,
    leaf: &X509,
    intermediates: &[X509],
    private_key: &PKey<Private>,
) -> Result<(), TlsBuildError> {
    let mut acceptor =
        SslAcceptor::mozilla_intermediate_v5(SslMethod::tls()).map_err(|source| {
            TlsBuildError::AcceptorPreflight {
                certificate: name.into(),
                source,
            }
        })?;
    acceptor
        .set_certificate(leaf)
        .and_then(|()| acceptor.set_private_key(private_key))
        .and_then(|()| {
            for intermediate in intermediates {
                acceptor.add_extra_chain_cert(intermediate.clone())?;
            }
            acceptor.check_private_key()
        })
        .map_err(|source| TlsBuildError::AcceptorPreflight {
            certificate: name.into(),
            source,
        })
}

fn metadata(
    name: &str,
    leaf: &X509,
    intermediates: &[X509],
    dns_names: Vec<String>,
) -> Result<CertificateMetadata, TlsBuildError> {
    let fingerprint = leaf.digest(MessageDigest::sha256()).map_err(|source| {
        TlsBuildError::CertificateMetadata {
            certificate: name.into(),
            source,
        }
    })?;
    let mut revision_material = Vec::new();
    for certificate in std::iter::once(leaf).chain(intermediates) {
        let der = certificate
            .to_der()
            .map_err(|source| TlsBuildError::CertificateMetadata {
                certificate: name.into(),
                source,
            })?;
        revision_material.extend_from_slice(&(der.len() as u64).to_be_bytes());
        revision_material.extend_from_slice(&der);
    }

    Ok(CertificateMetadata {
        name: name.into(),
        fingerprint_sha256: lower_hex(&fingerprint),
        revision: lower_hex(&sha256(&revision_material)),
        dns_names,
        validity: CertificateValidity {
            not_before: leaf.not_before().to_string(),
            not_after: leaf.not_after().to_string(),
        },
        intermediate_count: intermediates.len(),
    })
}
