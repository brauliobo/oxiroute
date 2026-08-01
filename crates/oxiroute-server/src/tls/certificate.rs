use std::{
    cmp::Ordering,
    collections::{BTreeMap, HashSet},
    fmt,
    net::IpAddr,
    path::Path,
    sync::Arc,
};

use super::{
    CertificateIdentitySan, CertificateIdentitySans, MAX_CERTIFICATE_CHAIN_BYTES,
    MAX_CERTIFICATES_IN_CHAIN, MAX_PRIVATE_KEY_BYTES, TlsBuildError, certificate_identity_sans,
    certificate_is_ca_capable, pem_labels, read_bounded_stable,
};
use crate::encoding::lower_hex;
use arc_swap::ArcSwap;
use async_trait::async_trait;
use openssl::{
    asn1::Asn1Time,
    error::ErrorStack,
    hash::MessageDigest,
    pkey::{Id, PKey, Private},
    sha::sha256,
    ssl::{NameType, SslAcceptor, SslMethod, SslOptions, SslSessionCacheMode, SslVersion},
    stack::Stack,
    x509::{
        X509, X509PurposeId, X509StoreContext, store::X509StoreBuilder, verify::X509VerifyFlags,
    },
};
use oxiroute_config::{AlpnProtocol, TlsProfile, TlsVersion};
use pingora::{
    listeners::{ALPN, TlsAccept, tls::TlsSettings},
    protocols::tls::TlsRef,
    tls::ext::{ssl_add_chain_cert, ssl_use_certificate, ssl_use_private_key},
};
use x509_parser::parse_x509_certificate;

const CERTIFICATE_FILE: &str = "certificate chain";
const PRIVATE_KEY_FILE: &str = "private key";
const MAX_DNS_NAMES: usize = 100;

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

pub struct TlsProfilePlan {
    name: String,
    min_version: TlsVersion,
    alpn: ALPN,
    selector: Arc<CertificateSelector>,
}

impl TlsProfilePlan {
    pub(crate) fn from_config(
        profile: &TlsProfile,
        active_generations: BTreeMap<String, Arc<ActiveCertificateGeneration>>,
    ) -> Result<Self, TlsBuildError> {
        let alpn = compile_alpn(&profile.name, &profile.alpn)?;
        let selector = Arc::new(CertificateSelector::new(profile, active_generations)?);
        Ok(Self {
            name: profile.name.clone(),
            min_version: profile.min_version,
            alpn,
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
        settings.set_session_cache_mode(SslSessionCacheMode::OFF);
        settings.set_options(SslOptions::NO_TICKET);
        settings.set_alpn(self.alpn.clone());
        Ok(settings)
    }
}

impl fmt::Debug for TlsProfilePlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TlsProfilePlan")
            .field("name", &self.name)
            .field("min_version", &self.min_version)
            .field("alpn", &self.alpn)
            .field("default_certificate", &self.selector.default_certificate)
            .field("certificates", &self.selector.active_generations.keys())
            .finish()
    }
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

#[async_trait]
impl TlsAccept for GenerationTlsAccept {
    async fn certificate_callback(&self, ssl: &mut TlsRef) {
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

fn compile_alpn(profile: &str, protocols: &[AlpnProtocol]) -> Result<ALPN, TlsBuildError> {
    match protocols {
        [AlpnProtocol::Http11] => Ok(ALPN::H1),
        [AlpnProtocol::H2] => Ok(ALPN::H2),
        [AlpnProtocol::H2, AlpnProtocol::Http11] => Ok(ALPN::H2H1),
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
