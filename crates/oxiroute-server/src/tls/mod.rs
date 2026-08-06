use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read},
    net::{IpAddr, Ipv4Addr, Ipv6Addr},
    path::{Path, PathBuf},
    sync::Arc,
};

use oxiroute_acme::{ChallengeStore, Dns01ProviderRegistry, StateStore};
use oxiroute_config::{CertificateSource, Config};
#[cfg(unix)]
use rustix::fs::{self as rustix_fs, Mode, OFlags};
use zeroize::Zeroizing;

mod acme;
mod certbot;
mod certbot_reconcile;
mod certbot_watcher;
mod certificate;
mod file_reconcile;
mod file_watcher;
mod tls_alpn;
mod upstream;

pub use acme::{
    AcmeDnsCleanupRecovery, AcmeManagedError, AcmeManagedOutcome, AcmeManagedPolicy,
    AcmeManagedReconciler, AcmeManagedStatus,
};
pub use certbot::{CertbotCandidate, CertbotLineage};
pub use certbot_reconcile::{
    CertbotActivationDirection, CertbotReconcileError, CertbotReconcileOutcome, CertbotReconciler,
    CertbotReconcilerStatus,
};
pub use certbot_watcher::{
    CertbotWatcherConfig, CertbotWatcherError, CertbotWatcherMonitor, CertbotWatcherStatus,
    CertbotWatcherSupervisor,
};
pub use certificate::{
    ActiveCertificateGeneration, CertificateGeneration, CertificateMetadata,
    CertificatePublishError, CertificateValidity, TlsProfilePlan,
};
pub use file_reconcile::{
    FileReconcileError, FileReconcileOutcome, FileReconciler, FileReconcilerStatus,
};
pub use file_watcher::{
    FileWatcherConfig, FileWatcherError, FileWatcherMonitor, FileWatcherStatus,
    FileWatcherSupervisor,
};
pub use tls_alpn::{
    TlsAlpnChallenge, TlsAlpnChallengeError, TlsAlpnChallengeIdentity, TlsAlpnChallengeLease,
    TlsAlpnChallengeStore, TLS_ALPN_IDENTIFIER_OID, TLS_ALPN_PROTOCOL,
};
pub use upstream::{prepare_upstream_tls, UpstreamTlsPlan};

pub const MAX_CERTIFICATE_CHAIN_BYTES: usize = 1024 * 1024;
pub const MAX_PRIVATE_KEY_BYTES: usize = 256 * 1024;
pub const MAX_DH_PARAMETERS_BYTES: usize = 64 * 1024;
pub const MAX_CA_CERTIFICATE_BYTES: usize = 1024 * 1024;
pub const MAX_CERTIFICATES_IN_CHAIN: usize = 16;
pub const MAX_CLIENT_CA_CERTIFICATES: usize = 128;

pub type CertificateIdentityMap = BTreeMap<String, Arc<ActiveCertificateGeneration>>;
pub type TlsProfilePlanMap = BTreeMap<String, Arc<TlsProfilePlan>>;

/// A fully prepared TLS snapshot. Its maps are immutable; Certbot-backed certificate slots can
/// publish validated replacement generations through their process-lifetime reconciler.
pub struct PreparedTls {
    certificates: CertificateIdentityMap,
    acme_reconcilers: Vec<Arc<AcmeManagedReconciler>>,
    challenge_store: ChallengeStore,
    tls_alpn_challenge_store: tls_alpn::TlsAlpnChallengeStore,
    certbot_reconcilers: Vec<Arc<CertbotReconciler>>,
    file_reconcilers: Vec<Arc<FileReconciler>>,
    profiles: TlsProfilePlanMap,
}

impl PreparedTls {
    #[must_use]
    pub const fn certificates(&self) -> &CertificateIdentityMap {
        &self.certificates
    }

    #[must_use]
    pub fn certbot_reconcilers(&self) -> &[Arc<CertbotReconciler>] {
        &self.certbot_reconcilers
    }

    #[must_use]
    pub fn acme_reconcilers(&self) -> &[Arc<AcmeManagedReconciler>] {
        &self.acme_reconcilers
    }

    #[must_use]
    pub const fn challenge_store(&self) -> &ChallengeStore {
        &self.challenge_store
    }

    #[must_use]
    pub const fn tls_alpn_challenge_store(&self) -> &tls_alpn::TlsAlpnChallengeStore {
        &self.tls_alpn_challenge_store
    }

    #[must_use]
    pub fn file_reconcilers(&self) -> &[Arc<FileReconciler>] {
        &self.file_reconcilers
    }

    #[must_use]
    pub const fn profiles(&self) -> &TlsProfilePlanMap {
        &self.profiles
    }

    /// Starts the production Certbot watcher for the reconcilers prepared from this snapshot.
    ///
    /// Direct-file identities remain startup snapshots and are deliberately not registered with
    /// this watcher.
    ///
    /// # Errors
    ///
    /// Returns an error when watcher configuration, directory setup, backend installation, or
    /// worker startup fails.
    pub fn start_certbot_watcher(
        &self,
        config: CertbotWatcherConfig,
    ) -> Result<Option<CertbotWatcherSupervisor>, CertbotWatcherError> {
        CertbotWatcherSupervisor::start_if_configured(self.certbot_reconcilers.clone(), config)
    }

    /// Checks that the production Certbot notification backend can watch this snapshot without
    /// starting a reconciliation worker.
    ///
    /// # Errors
    ///
    /// Returns an error when watcher configuration, directory setup, or backend installation fails.
    pub fn check_certbot_watcher(
        &self,
        config: CertbotWatcherConfig,
    ) -> Result<(), CertbotWatcherError> {
        CertbotWatcherSupervisor::check_if_configured(&self.certbot_reconcilers, config)
    }

    /// Checks that the direct-file notification backend can watch this snapshot without starting
    /// a reconciliation worker.
    ///
    /// # Errors
    ///
    /// Returns an error when watcher configuration, directory setup, or backend installation fails.
    pub fn check_file_watcher(&self, config: FileWatcherConfig) -> Result<(), FileWatcherError> {
        FileWatcherSupervisor::check_if_configured(&self.file_reconcilers, config)
    }

    /// Starts the direct-file certificate watcher when at least one file identity is configured.
    ///
    /// # Errors
    ///
    /// Returns an error when watcher configuration, directory setup, backend installation, or
    /// worker startup fails.
    pub fn start_file_watcher(
        &self,
        config: FileWatcherConfig,
    ) -> Result<Option<FileWatcherSupervisor>, FileWatcherError> {
        FileWatcherSupervisor::start_if_configured(self.file_reconcilers.clone(), config)
    }
}

impl std::fmt::Debug for PreparedTls {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedTls")
            .field("certificates", &self.certificates)
            .field("acme_reconcilers", &self.acme_reconcilers)
            .field("challenge_store", &self.challenge_store)
            .field("tls_alpn_challenge_store", &self.tls_alpn_challenge_store)
            .field("certbot_reconcilers", &self.certbot_reconcilers)
            .field("file_reconcilers", &self.file_reconcilers)
            .field("profiles", &self.profiles)
            .finish()
    }
}

/// Loads every configured file identity and resolves every TLS profile into one immutable plan.
///
/// This does not establish that an imported chain is publicly trusted. It only validates that the
/// configured material is usable by the local OpenSSL acceptor.
///
/// # Errors
///
/// Returns a typed error when a file is unstable or invalid, OpenSSL rejects an identity, or a
/// profile cannot be resolved.
#[allow(clippy::too_many_lines)]
pub fn prepare_tls(config: &Config) -> Result<PreparedTls, TlsBuildError> {
    prepare_tls_with_dns01_providers(config, Dns01ProviderRegistry::default())
}

/// Loads TLS state with an explicit registry of statically linked DNS-01 providers.
///
/// The default [`prepare_tls`] path has no dynamic provider discovery, so a DNS-01 certificate
/// fails closed unless an embedding caller supplies an exact allowlisted implementation here.
///
/// # Errors
///
/// Returns the same build errors as [`prepare_tls`], plus an unsupported-provider error when a
/// configured DNS-01 provider is not registered.
#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
pub fn prepare_tls_with_dns01_providers(
    config: &Config,
    dns01_providers: Dns01ProviderRegistry,
) -> Result<PreparedTls, TlsBuildError> {
    let mut certificates = BTreeMap::new();
    let mut acme_reconcilers = Vec::new();
    let mut acme_states = BTreeMap::new();
    let challenge_store = ChallengeStore::default();
    let tls_alpn_challenge_store = tls_alpn::TlsAlpnChallengeStore::default();
    let mut certbot_reconcilers = Vec::new();
    let mut file_reconcilers = Vec::new();
    for certificate in &config.certificates {
        let name = certificate.name.clone();
        let (
            generation,
            certbot,
            direct_files,
            managed_state,
            managed_policy,
            dns_provider,
        ) =
            match &certificate.source {
                CertificateSource::Files {
                    certificate_chain_path,
                    private_key_path,
                } => (
                    CertificateGeneration::from_files(
                        certificate.name.clone(),
                        &certificate.dns_names,
                        certificate_chain_path,
                        private_key_path,
                    )?,
                    None,
                    Some((certificate_chain_path.clone(), private_key_path.clone())),
                    None,
                    None,
                    None,
                ),
                CertificateSource::Certbot {
                    live_directory_path,
                    archive_directory_path,
                } => {
                    let lineage = CertbotLineage::new(live_directory_path, archive_directory_path);
                    let candidate = lineage.load_candidate(name.clone(), &certificate.dns_names)?;
                    let archive_revision = candidate.archive_revision();
                    (
                        candidate.into_generation(),
                        Some((lineage, archive_revision)),
                        None,
                        None,
                        None,
                        None,
                    )
                }
                CertificateSource::AcmeManaged {
                    directory_url,
                    contacts,
                    terms_agreed,
                    challenge,
                    key_type,
                    allowed_dns_suffixes,
                    retained_revisions,
                    retention_days,
                    state_root,
                    dns01,
                    ..
                } => {
                    let dns_provider = if *challenge == oxiroute_config::AcmeChallengeType::Dns01 {
                        let provider_name = dns01
                            .as_ref()
                            .map(|dns01| dns01.provider.as_str())
                            .unwrap_or_default();
                        Some(dns01_providers.get(provider_name).ok_or_else(|| {
                            TlsBuildError::DnsProviderUnsupported {
                                certificate: name.clone(),
                                provider: provider_name.into(),
                            }
                        })?)
                    } else {
                        None
                    };
                    let state = if let Some(state) = acme_states.get(state_root) {
                        Arc::clone(state)
                    } else {
                        let state = Arc::new(StateStore::open(state_root).map_err(|source| {
                            TlsBuildError::AcmeState {
                                certificate: name.clone(),
                                source: Box::new(source),
                            }
                        })?);
                        acme_states.insert(state_root.clone(), Arc::clone(&state));
                        state
                    };
                    let (
                        generation,
                        revisions,
                        disk_revision,
                        not_before,
                        not_after,
                        initial_issuance_due,
                    ) = AcmeManagedReconciler::load(
                        name.clone(),
                        &certificate.dns_names,
                        state,
                        *key_type,
                    )?;
                    (
                        generation,
                        None,
                        None,
                        Some((
                            revisions,
                            disk_revision,
                            not_before,
                            not_after,
                            initial_issuance_due,
                        )),
                        Some(AcmeManagedPolicy {
                            directory_url: directory_url.clone(),
                            contacts: contacts.clone(),
                            terms_agreed: *terms_agreed,
                            challenge: *challenge,
                            key_type: *key_type,
                            allowed_dns_suffixes: allowed_dns_suffixes.clone(),
                            retained_revisions: *retained_revisions,
                            retention_days: *retention_days,
                            dns01: dns01.clone(),
                        }),
                        dns_provider,
                    )
                }
                CertificateSource::SelfSignedDevelopment {
                    validity_days,
                    key_type,
                } => (
                    CertificateGeneration::self_signed_development(
                        certificate.name.clone(),
                        &certificate.dns_names,
                        *validity_days,
                        *key_type,
                    )?,
                    None,
                    None,
                    None,
                    None,
                    None,
                ),
            };
        let generation = Arc::new(generation);
        let active = Arc::new(ActiveCertificateGeneration::new(generation));
        if certificates
            .insert(name.clone(), Arc::clone(&active))
            .is_some()
        {
            return Err(TlsBuildError::DuplicateCertificate { name });
        }
        if let Some((lineage, archive_revision)) = certbot {
            certbot_reconcilers.push(Arc::new(CertbotReconciler::new(
                lineage,
                name,
                certificate.dns_names.clone(),
                archive_revision,
                active,
            )));
        } else if let (
            Some((revisions, disk_revision, not_before, not_after, initial_issuance_due)),
            Some(policy),
        ) = (managed_state, managed_policy)
        {
            acme_reconcilers.push(Arc::new(AcmeManagedReconciler::new_with_challenge_stores(
                certificate.name.clone(),
                certificate.dns_names.clone(),
                policy,
                revisions,
                disk_revision,
                not_before,
                not_after,
                initial_issuance_due,
                challenge_store.clone(),
                tls_alpn_challenge_store.clone(),
                dns_provider,
                active,
            )));
        } else if let Some((certificate_chain_path, private_key_path)) = direct_files {
            file_reconcilers.push(Arc::new(FileReconciler::new(
                certificate.name.clone(),
                certificate.dns_names.clone(),
                certificate_chain_path,
                private_key_path,
                active,
            )));
        }
    }

    let mut profiles = BTreeMap::new();
    for profile in &config.tls_profiles {
        let mut active_generations = BTreeMap::new();
        for certificate_name in &profile.certificates {
            let active_generation = certificates.get(certificate_name).ok_or_else(|| {
                TlsBuildError::UnknownProfileCertificate {
                    profile: profile.name.clone(),
                    certificate: certificate_name.clone(),
                }
            })?;
            if active_generations
                .insert(certificate_name.clone(), Arc::clone(active_generation))
                .is_some()
            {
                return Err(TlsBuildError::DuplicateProfileCertificate {
                    profile: profile.name.clone(),
                    certificate: certificate_name.clone(),
                });
            }
        }
        let plan = Arc::new(TlsProfilePlan::from_config(
            profile,
            active_generations,
            tls_alpn_challenge_store.clone(),
        )?);
        if profiles.insert(profile.name.clone(), plan).is_some() {
            return Err(TlsBuildError::DuplicateTlsProfile {
                name: profile.name.clone(),
            });
        }
    }

    Ok(PreparedTls {
        certificates,
        acme_reconcilers,
        challenge_store,
        tls_alpn_challenge_store,
        certbot_reconcilers,
        file_reconcilers,
        profiles,
    })
}

#[derive(Debug, thiserror::Error)]
pub enum TlsBuildError {
    #[error("managed ACME certificate `{certificate}` state is unavailable or invalid")]
    AcmeState {
        certificate: String,
        #[source]
        source: Box<oxiroute_acme::AcmeStateError>,
    },
    #[error("managed ACME certificate `{certificate}` uses unsupported DNS-01 provider `{provider}`")]
    DnsProviderUnsupported {
        certificate: String,
        provider: String,
    },
    #[error("managed ACME certificate `{certificate}` could not be published")]
    AcmePublication {
        certificate: String,
        #[source]
        source: CertificatePublishError,
    },
    #[error("failed to open {kind} file `{path}` for `{owner}`")]
    FileOpen {
        owner: String,
        kind: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("failed to inspect {kind} file `{path}` for `{owner}`")]
    FileMetadata {
        owner: String,
        kind: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{kind} path `{path}` for `{owner}` is not a regular file")]
    NotRegularFile {
        owner: String,
        kind: &'static str,
        path: PathBuf,
    },
    #[error("{kind} file `{path}` for `{owner}` exceeds the {limit}-byte limit")]
    FileTooLarge {
        owner: String,
        kind: &'static str,
        path: PathBuf,
        limit: usize,
    },
    #[error("failed to read {kind} file `{path}` for `{owner}`")]
    FileRead {
        owner: String,
        kind: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{kind} file `{path}` for `{owner}` changed while it was read")]
    FileChanged {
        owner: String,
        kind: &'static str,
        path: PathBuf,
    },
    #[error("development certificate `{certificate}` validity is outside the supported bounds")]
    InvalidSelfSignedValidity { certificate: String },
    #[error("failed to generate the development certificate key for `{certificate}`")]
    SelfSignedKeyGeneration {
        certificate: String,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error("failed to generate the development certificate for `{certificate}`")]
    SelfSignedCertificateGeneration {
        certificate: String,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error("development certificate `{certificate}` is not self-signed")]
    SelfSignedSignatureMismatch { certificate: String },
    #[error("{kind} file `{path}` for `{owner}` is empty")]
    EmptyFile {
        owner: String,
        kind: &'static str,
        path: PathBuf,
    },
    #[error("failed to resolve Certbot {kind} directory `{path}` for `{certificate}`")]
    CertbotDirectoryCanonicalization {
        certificate: String,
        kind: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("Certbot {kind} path `{path}` for `{certificate}` is not a directory")]
    CertbotPathNotDirectory {
        certificate: String,
        kind: &'static str,
        path: PathBuf,
    },
    #[error("Certbot live and archive directories for `{certificate}` resolve to `{path}`")]
    DuplicateCertbotDirectories { certificate: String, path: PathBuf },
    #[error("failed to inspect Certbot live link `{path}` for `{certificate}`")]
    CertbotLiveLinkMetadata {
        certificate: String,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("Certbot live entry `{path}` for `{certificate}` is not a symlink")]
    CertbotLiveEntryNotSymlink { certificate: String, path: PathBuf },
    #[error("failed to read Certbot live link `{path}` for `{certificate}`")]
    CertbotLiveLinkRead {
        certificate: String,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("Certbot live link `{path}` for `{certificate}` has invalid target `{target}`")]
    InvalidCertbotLiveLinkTarget {
        certificate: String,
        path: PathBuf,
        target: PathBuf,
    },
    #[error("Certbot live links for `{certificate}` mix archive revisions {expected} and {found}")]
    MixedCertbotArchiveRevisions {
        certificate: String,
        expected: u64,
        found: u64,
    },
    #[error("failed to inspect {kind} archive entry `{path}` for `{certificate}`")]
    CertbotArchiveEntryMetadata {
        certificate: String,
        kind: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("{kind} archive entry `{path}` for `{certificate}` is not a regular file")]
    CertbotArchiveEntryNotRegular {
        certificate: String,
        kind: &'static str,
        path: PathBuf,
    },
    #[error("failed to read Certbot archive private-key link `{path}` for `{certificate}`")]
    CertbotArchivePrivateKeyLinkRead {
        certificate: String,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error(
        "Certbot archive private-key link `{path}` for `{certificate}` has invalid target `{target}`"
    )]
    InvalidCertbotArchivePrivateKeyLink {
        certificate: String,
        path: PathBuf,
        target: PathBuf,
    },
    #[error("Certbot full chain `{path}` for `{certificate}` does not equal cert.pem + chain.pem")]
    CertbotFullchainMismatch { certificate: String, path: PathBuf },
    #[error("Certbot lineage for `{certificate}` changed while it was read")]
    CertbotLineageChanged { certificate: String },
    #[error(
        "private key file `{path}` for `{certificate}` must use mode 0400, 0600, 0440, or 0640"
    )]
    InsecurePrivateKeyPermissions { certificate: String, path: PathBuf },
    #[error("{kind} file `{path}` for `{owner}` has an invalid PEM envelope: {detail}")]
    InvalidPem {
        owner: String,
        kind: &'static str,
        path: PathBuf,
        detail: &'static str,
    },
    #[error(
        "certificate chain for `{certificate}` contains {count} certificates; maximum is {MAX_CERTIFICATES_IN_CHAIN}"
    )]
    TooManyChainCertificates { certificate: String, count: usize },
    #[error("failed to parse certificate chain `{path}` for `{certificate}`")]
    CertificateParse {
        certificate: String,
        path: PathBuf,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error(
        "private key file `{path}` for `{certificate}` must contain one unencrypted private key"
    )]
    EncryptedPrivateKey { certificate: String, path: PathBuf },
    #[error("private key file `{path}` for `{certificate}` must contain exactly one private key")]
    PrivateKeyCount { certificate: String, path: PathBuf },
    #[error("failed to parse private key `{path}` for `{certificate}`")]
    PrivateKeyParse {
        certificate: String,
        path: PathBuf,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error("failed to read the leaf public key for certificate `{certificate}`")]
    LeafPublicKey {
        certificate: String,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error("leaf certificate and private key do not match for `{certificate}`")]
    PrivateKeyMismatch { certificate: String },
    #[error("certificate `{certificate}` must use an RSA or EC private key")]
    UnsupportedPrivateKeyAlgorithm { certificate: String },
    #[error(
        "certificate `{certificate}` private key has {bits} bits; minimum for its algorithm is {minimum_bits}"
    )]
    PrivateKeyTooWeak {
        certificate: String,
        bits: u32,
        minimum_bits: u32,
    },
    #[error("failed to encode certificate `{certificate}` for key-usage validation")]
    CertificateKeyUsageEncoding {
        certificate: String,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error("failed to inspect key usage for certificate `{certificate}`")]
    CertificateKeyUsageInspection { certificate: String },
    #[error("certificate `{certificate}` key usage must permit digital signatures")]
    MissingDigitalSignatureKeyUsage { certificate: String },
    #[error("certificate `{certificate}` is not currently valid (not before {not_before})")]
    CertificateNotYetValid {
        certificate: String,
        not_before: String,
    },
    #[error("certificate `{certificate}` is not currently valid (expired at {not_after})")]
    CertificateExpired {
        certificate: String,
        not_after: String,
    },
    #[error("failed to evaluate validity for certificate `{certificate}`")]
    CertificateValidity {
        certificate: String,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error(
        "certificate `{certificate}` must contain at least one DNS or IP subject alternative name"
    )]
    MissingDnsSan { certificate: String },
    #[error("certificate `{certificate}` has invalid declared DNS/IP identities")]
    InvalidDeclaredDnsNames { certificate: String },
    #[error("failed to inspect DNS/IP subject alternative names for certificate `{certificate}`")]
    DnsSanInspection {
        certificate: String,
        #[source]
        source: Box<openssl::error::ErrorStack>,
    },
    #[error("certificate `{certificate}` contains a malformed DNS/IP subject alternative name")]
    InvalidDnsSanEncoding { certificate: String },
    #[error(
        "certificate `{certificate}` contains invalid DNS subject alternative name `{dns_name}`"
    )]
    InvalidDnsSan {
        certificate: String,
        dns_name: String,
    },
    #[error("certificate `{certificate}` does not contain every declared DNS/IP identity")]
    DnsSanMismatch { certificate: String },
    #[error("certificate `{certificate}` must supply at least one issuer after the leaf")]
    IncompleteCertificateChain { certificate: String },
    #[error(
        "certificate `{certificate}` chain entry {index} is not currently valid (not before {not_before})"
    )]
    ChainCertificateNotYetValid {
        certificate: String,
        index: usize,
        not_before: String,
    },
    #[error(
        "certificate `{certificate}` chain entry {index} is not currently valid (expired at {not_after})"
    )]
    ChainCertificateExpired {
        certificate: String,
        index: usize,
        not_after: String,
    },
    #[error(
        "certificate `{certificate}` chain entry {issuer_index} is not the issuer of entry {subject_index}: {reason}"
    )]
    InvalidChainIssuer {
        certificate: String,
        subject_index: usize,
        issuer_index: usize,
        reason: openssl::x509::X509VerifyResult,
    },
    #[error("certificate `{certificate}` chain entry {issuer_index} has an invalid public key")]
    ChainIssuerPublicKey {
        certificate: String,
        issuer_index: usize,
        #[source]
        source: Box<openssl::error::ErrorStack>,
    },
    #[error(
        "certificate `{certificate}` chain entry {subject_index} signature does not match issuer entry {issuer_index}"
    )]
    InvalidChainSignature {
        certificate: String,
        subject_index: usize,
        issuer_index: usize,
    },
    #[error(
        "failed to validate certificate `{certificate}` chain entry {subject_index} signature"
    )]
    ChainSignatureValidation {
        certificate: String,
        subject_index: usize,
        #[source]
        source: Box<openssl::error::ErrorStack>,
    },
    #[error("certificate `{certificate}` chain issuer entry {index} is not CA-capable")]
    NonCaChainIssuer { certificate: String, index: usize },
    #[error("failed to inspect certificate `{certificate}` chain issuer entry {index}")]
    ChainIssuerInspection {
        certificate: String,
        index: usize,
        #[source]
        source: Box<openssl::error::ErrorStack>,
    },
    #[error("failed to construct strict verification for certificate `{certificate}`")]
    ChainVerificationSetup {
        certificate: String,
        #[source]
        source: Box<openssl::error::ErrorStack>,
    },
    #[error(
        "strict SSL-server verification rejected certificate `{certificate}` at chain depth {depth}: {reason}"
    )]
    ChainVerification {
        certificate: String,
        depth: u32,
        reason: openssl::x509::X509VerifyResult,
    },
    #[error("strict verification produced an incomplete chain for certificate `{certificate}`")]
    IncompleteVerifiedChain { certificate: String },
    #[error("failed to derive public metadata for certificate `{certificate}`")]
    CertificateMetadata {
        certificate: String,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error("OpenSSL acceptor preflight rejected certificate `{certificate}`")]
    AcceptorPreflight {
        certificate: String,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error("duplicate certificate identity `{name}`")]
    DuplicateCertificate { name: String },
    #[error("TLS profile `{profile}` references unknown certificate `{certificate}`")]
    UnknownProfileCertificate {
        profile: String,
        certificate: String,
    },
    #[error("TLS profile `{profile}` references certificate `{certificate}` more than once")]
    DuplicateProfileCertificate {
        profile: String,
        certificate: String,
    },
    #[error(
        "TLS profile `{profile}` default certificate `{certificate}` is not in its certificate list"
    )]
    ProfileDefaultNotListed {
        profile: String,
        certificate: String,
    },
    #[error(
        "TLS profile `{profile}` assigns DNS name `{dns_name}` to both `{first_certificate}` and `{second_certificate}`"
    )]
    OverlappingProfileDnsName {
        profile: String,
        dns_name: String,
        first_certificate: String,
        second_certificate: String,
    },
    #[error("TLS profile `{profile}` has an unsupported ALPN policy")]
    InvalidProfileAlpn { profile: String },
    #[error("duplicate TLS profile identity `{name}`")]
    DuplicateTlsProfile { name: String },
    #[error("failed to create Pingora TLS settings for profile `{profile}`")]
    TlsSettings {
        profile: String,
        #[source]
        source: Box<pingora::Error>,
    },
    #[error("failed to initialize TLS-ALPN-01 certificate selection: {detail}")]
    TlsAlpnSelectionIndex { detail: String },
    #[error("failed to apply OpenSSL settings for TLS profile `{profile}`")]
    TlsProfileSettings {
        profile: String,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error("failed to parse DH parameters `{path}` for TLS profile `{profile}`")]
    TlsDhParameters {
        profile: String,
        path: PathBuf,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error("TLS profile `{profile}` has runtime TLS policy values outside OpenSSL limits")]
    InvalidTlsProfilePolicy { profile: String },
    #[error("TLS profile `{profile}` has invalid client-auth policy: {detail}")]
    InvalidTlsClientAuthPolicy {
        profile: String,
        detail: &'static str,
    },
    #[error("TLS profile `{profile}` client CA bundle contains too many certificates")]
    TooManyClientCaCertificates { profile: String },
    #[error("failed to parse client CA bundle `{path}` for TLS profile `{profile}`")]
    ClientCaParse {
        profile: String,
        path: PathBuf,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error("client CA certificate {index} for TLS profile `{profile}` is not currently valid")]
    ClientCaCertificateInvalid {
        profile: String,
        index: usize,
        detail: &'static str,
    },
    #[error("client CA certificate {index} for TLS profile `{profile}` is not CA-capable")]
    NonCaClientCertificate { profile: String, index: usize },
    #[error("client CA certificate {index} for TLS profile `{profile}` could not be inspected")]
    ClientCaCertificateInspection {
        profile: String,
        index: usize,
        #[source]
        source: Box<openssl::error::ErrorStack>,
    },
    #[error("client CA certificate {index} for TLS profile `{profile}` is duplicated")]
    DuplicateClientCaCertificate { profile: String, index: usize },
    #[error(
        "client CA certificate {index} for TLS profile `{profile}` is not usable by rustls: {detail}"
    )]
    ClientCaRustlsCertificate {
        profile: String,
        index: usize,
        detail: String,
    },
    #[error("failed to construct the client CA trust store for TLS profile `{profile}`")]
    ClientCaStore {
        profile: String,
        #[source]
        source: Box<openssl::error::ErrorStack>,
    },
    #[error("failed to construct the rustls client certificate verifier for TLS profile `{profile}`: {detail}")]
    ClientCaRustlsVerifier { profile: String, detail: String },
    #[error("upstream pool `{pool}` has invalid TLS server name `{server_name}`")]
    InvalidUpstreamServerName { pool: String, server_name: String },
    #[error("upstream pool `{pool}` has an invalid HTTP version range")]
    InvalidHttpVersionRange { pool: String },
    #[error("custom CA bundle for upstream pool `{pool}` contains too many certificates")]
    TooManyCaCertificates { pool: String },
    #[error("custom CA bundle for upstream pool `{pool}` contains duplicate certificate {index}")]
    DuplicateCaCertificate { pool: String, index: usize },
    #[error(
        "custom CA certificate {index} for upstream pool `{pool}` is not currently valid (not before {not_before})"
    )]
    CaCertificateNotYetValid {
        pool: String,
        index: usize,
        not_before: String,
    },
    #[error(
        "custom CA certificate {index} for upstream pool `{pool}` is not currently valid (expired at {not_after})"
    )]
    CaCertificateExpired {
        pool: String,
        index: usize,
        not_after: String,
    },
    #[error("custom CA certificate {index} for upstream pool `{pool}` is not CA-capable")]
    NonCaCertificate { pool: String, index: usize },
    #[error("failed to inspect custom CA certificate {index} for upstream pool `{pool}`")]
    CaCertificateInspection {
        pool: String,
        index: usize,
        #[source]
        source: Box<openssl::error::ErrorStack>,
    },
    #[error("failed to construct custom CA trust store for upstream pool `{pool}`")]
    CaStore {
        pool: String,
        #[source]
        source: Box<openssl::error::ErrorStack>,
    },
    #[error("failed to parse custom CA bundle `{path}` for upstream pool `{pool}`")]
    CaParse {
        pool: String,
        path: PathBuf,
        #[source]
        source: openssl::error::ErrorStack,
    },
    #[error("failed to serialize custom CA policy for upstream pool `{pool}`")]
    CaPolicy {
        pool: String,
        #[source]
        source: openssl::error::ErrorStack,
    },
}

pub(crate) fn read_bounded_stable(
    owner: &str,
    kind: &'static str,
    path: &Path,
    limit: usize,
    private_key: bool,
) -> Result<Zeroizing<Vec<u8>>, TlsBuildError> {
    let first = read_bounded_once(owner, kind, path, limit, private_key)?;
    let second = read_bounded_once(owner, kind, path, limit, private_key)?;
    if first.as_slice() != second.as_slice() {
        return Err(TlsBuildError::FileChanged {
            owner: owner.into(),
            kind,
            path: path.into(),
        });
    }
    Ok(first)
}

fn read_bounded_once(
    owner: &str,
    kind: &'static str,
    path: &Path,
    limit: usize,
    private_key: bool,
) -> Result<Zeroizing<Vec<u8>>, TlsBuildError> {
    let mut file = open_read_no_follow(path).map_err(|source| TlsBuildError::FileOpen {
        owner: owner.into(),
        kind,
        path: path.into(),
        source,
    })?;
    let metadata = file
        .metadata()
        .map_err(|source| TlsBuildError::FileMetadata {
            owner: owner.into(),
            kind,
            path: path.into(),
            source,
        })?;
    if !metadata.file_type().is_file() {
        return Err(TlsBuildError::NotRegularFile {
            owner: owner.into(),
            kind,
            path: path.into(),
        });
    }
    if metadata.len() > limit as u64 {
        return Err(TlsBuildError::FileTooLarge {
            owner: owner.into(),
            kind,
            path: path.into(),
            limit,
        });
    }
    #[cfg(unix)]
    if private_key {
        use std::os::unix::fs::PermissionsExt as _;

        let mode = metadata.permissions().mode() & 0o7777;
        if !matches!(mode, 0o400 | 0o600 | 0o440 | 0o640) {
            return Err(TlsBuildError::InsecurePrivateKeyPermissions {
                certificate: owner.into(),
                path: path.into(),
            });
        }
    }
    #[cfg(not(unix))]
    let _ = private_key;

    let capacity = usize::try_from(metadata.len()).map_err(|_| TlsBuildError::FileTooLarge {
        owner: owner.into(),
        kind,
        path: path.into(),
        limit,
    })?;
    let mut bytes = Zeroizing::new(Vec::with_capacity(capacity));
    file.by_ref()
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|source| TlsBuildError::FileRead {
            owner: owner.into(),
            kind,
            path: path.into(),
            source,
        })?;
    if bytes.len() > limit {
        return Err(TlsBuildError::FileTooLarge {
            owner: owner.into(),
            kind,
            path: path.into(),
            limit,
        });
    }

    if bytes.is_empty() {
        return Err(TlsBuildError::EmptyFile {
            owner: owner.into(),
            kind,
            path: path.into(),
        });
    }
    Ok(bytes)
}

fn open_read_no_follow(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        rustix_fs::open(
            path,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(Into::into)
    }

    #[cfg(not(unix))]
    {
        File::open(path)
    }
}

pub(crate) fn certificate_is_ca_capable(
    certificate: &openssl::x509::X509Ref,
) -> Result<bool, openssl::error::ErrorStack> {
    let der = certificate.to_der()?;
    Ok(
        ca_extensions(&der).is_some_and(|(basic_constraints, key_usage)| {
            basic_constraints && key_usage.unwrap_or(true)
        }),
    )
}

pub(crate) enum CertificateIdentitySans {
    Missing,
    Malformed,
    Names(Vec<CertificateIdentitySan>),
}

pub(crate) enum CertificateIdentitySan {
    Dns(Vec<u8>),
    Ip(IpAddr),
}

pub(crate) fn certificate_identity_sans(
    certificate: &openssl::x509::X509Ref,
) -> Result<CertificateIdentitySans, openssl::error::ErrorStack> {
    const SUBJECT_ALT_NAME_OID: &[u8] = &[0x55, 0x1d, 0x11];

    let der = certificate.to_der()?;
    let mut extensions = match certificate_extensions(&der) {
        ParsedExtensions::Missing => return Ok(CertificateIdentitySans::Missing),
        ParsedExtensions::Malformed => return Ok(CertificateIdentitySans::Malformed),
        ParsedExtensions::Present(extensions) => extensions,
    };
    let mut identities = None;
    while !extensions.is_empty() {
        let Some((oid, value)) = der_extension(&mut extensions) else {
            return Ok(CertificateIdentitySans::Malformed);
        };
        if oid != SUBJECT_ALT_NAME_OID {
            continue;
        }
        if identities.is_some() {
            return Ok(CertificateIdentitySans::Malformed);
        }
        let mut value = value;
        let Some(mut general_names) = der_value(&mut value, 0x30) else {
            return Ok(CertificateIdentitySans::Malformed);
        };
        if !value.is_empty() {
            return Ok(CertificateIdentitySans::Malformed);
        }
        let mut parsed_identities = Vec::new();
        while !general_names.is_empty() {
            let Some((tag, value)) = der_element(&mut general_names) else {
                return Ok(CertificateIdentitySans::Malformed);
            };
            match tag {
                0x82 => parsed_identities.push(CertificateIdentitySan::Dns(value.to_vec())),
                0x87 => {
                    let ip = match value {
                        [a, b, c, d] => IpAddr::V4(Ipv4Addr::new(*a, *b, *c, *d)),
                        value if value.len() == 16 => {
                            let octets: [u8; 16] = value
                                .try_into()
                                .expect("a 16-byte IP SAN is an IPv6 address");
                            IpAddr::V6(Ipv6Addr::from(octets))
                        }
                        _ => return Ok(CertificateIdentitySans::Malformed),
                    };
                    parsed_identities.push(CertificateIdentitySan::Ip(ip));
                }
                _ => {}
            }
        }
        identities = Some(parsed_identities);
    }
    Ok(identities.map_or(
        CertificateIdentitySans::Missing,
        CertificateIdentitySans::Names,
    ))
}

enum ParsedExtensions<'a> {
    Missing,
    Malformed,
    Present(&'a [u8]),
}

fn certificate_extensions(der: &[u8]) -> ParsedExtensions<'_> {
    let Some(certificate) = (|| {
        let mut certificate_der = der;
        let certificate = der_value(&mut certificate_der, 0x30)?;
        certificate_der.is_empty().then_some(certificate)
    })() else {
        return ParsedExtensions::Malformed;
    };

    let Some(mut tbs_certificate) = ({
        let mut certificate = certificate;
        der_value(&mut certificate, 0x30)
    }) else {
        return ParsedExtensions::Malformed;
    };
    if tbs_certificate.first() == Some(&0xa0) && der_element(&mut tbs_certificate).is_none() {
        return ParsedExtensions::Malformed;
    }
    for _ in 0..6 {
        if der_element(&mut tbs_certificate).is_none() {
            return ParsedExtensions::Malformed;
        }
    }

    while !tbs_certificate.is_empty() {
        let Some((tag, value)) = der_element(&mut tbs_certificate) else {
            return ParsedExtensions::Malformed;
        };
        if tag != 0xa3 {
            continue;
        }
        let mut explicit_extensions = value;
        let Some(extensions) = der_value(&mut explicit_extensions, 0x30) else {
            return ParsedExtensions::Malformed;
        };
        if !explicit_extensions.is_empty() || !tbs_certificate.is_empty() {
            return ParsedExtensions::Malformed;
        }
        return ParsedExtensions::Present(extensions);
    }
    ParsedExtensions::Missing
}

fn ca_extensions(der: &[u8]) -> Option<(bool, Option<bool>)> {
    let ParsedExtensions::Present(extensions) = certificate_extensions(der) else {
        return None;
    };
    parse_ca_extensions(extensions)
}

fn parse_ca_extensions(mut extensions: &[u8]) -> Option<(bool, Option<bool>)> {
    const BASIC_CONSTRAINTS_OID: &[u8] = &[0x55, 0x1d, 0x13];
    const KEY_USAGE_OID: &[u8] = &[0x55, 0x1d, 0x0f];

    let mut basic_constraints = None;
    let mut key_usage = None;
    while !extensions.is_empty() {
        let (oid, value) = der_extension(&mut extensions)?;

        match oid {
            BASIC_CONSTRAINTS_OID if basic_constraints.is_none() => {
                basic_constraints = Some(parse_basic_constraints(value)?);
            }
            KEY_USAGE_OID if key_usage.is_none() => {
                key_usage = Some(parse_key_usage(value)?);
            }
            BASIC_CONSTRAINTS_OID | KEY_USAGE_OID => return None,
            _ => {}
        }
    }

    basic_constraints.map(|basic_constraints| (basic_constraints, key_usage))
}

fn der_extension<'a>(extensions: &mut &'a [u8]) -> Option<(&'a [u8], &'a [u8])> {
    let mut extension = der_value(extensions, 0x30)?;
    let oid = der_value(&mut extension, 0x06)?;
    if extension.first() == Some(&0x01) {
        der_value(&mut extension, 0x01)?;
    }
    let value = der_value(&mut extension, 0x04)?;
    extension.is_empty().then_some((oid, value))
}

fn parse_basic_constraints(value: &[u8]) -> Option<bool> {
    let mut value = value;
    let mut constraints = der_value(&mut value, 0x30)?;
    if !value.is_empty() {
        return None;
    }
    if constraints.is_empty() {
        return Some(false);
    }
    let ca = der_value(&mut constraints, 0x01)?;
    (ca.len() == 1).then_some(ca[0] != 0)
}

fn parse_key_usage(value: &[u8]) -> Option<bool> {
    let mut value = value;
    let key_usage = der_value(&mut value, 0x03)?;
    if !value.is_empty() || key_usage.len() < 2 || key_usage[0] > 7 {
        return None;
    }
    Some(key_usage[1] & 0x04 != 0)
}

fn der_value<'a>(input: &mut &'a [u8], expected_tag: u8) -> Option<&'a [u8]> {
    let (tag, value) = der_element(input)?;
    (tag == expected_tag).then_some(value)
}

fn der_element<'a>(input: &mut &'a [u8]) -> Option<(u8, &'a [u8])> {
    let (&tag, remainder) = input.split_first()?;
    let (&length_byte, mut remainder) = remainder.split_first()?;
    let length = if length_byte & 0x80 == 0 {
        usize::from(length_byte)
    } else {
        let length_bytes = usize::from(length_byte & 0x7f);
        if length_bytes == 0 || length_bytes > size_of::<usize>() || remainder.len() < length_bytes
        {
            return None;
        }
        let (encoded_length, rest) = remainder.split_at(length_bytes);
        if encoded_length[0] == 0 {
            return None;
        }
        remainder = rest;
        encoded_length.iter().try_fold(0_usize, |length, byte| {
            length.checked_mul(256)?.checked_add(usize::from(*byte))
        })?
    };
    let (value, remainder) = remainder.split_at_checked(length)?;
    *input = remainder;
    Some((tag, value))
}

pub(crate) fn pem_labels<'a>(
    owner: &str,
    kind: &'static str,
    path: &Path,
    bytes: &'a [u8],
) -> Result<Vec<&'a str>, TlsBuildError> {
    let text = std::str::from_utf8(bytes).map_err(|_| TlsBuildError::InvalidPem {
        owner: owner.into(),
        kind,
        path: path.into(),
        detail: "PEM must be ASCII text",
    })?;
    let mut labels = Vec::new();
    let mut open_label = None;

    for raw_line in text.lines() {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if let Some(label) = open_label {
            let expected_end = format!("-----END {label}-----");
            if line == expected_end {
                labels.push(label);
                open_label = None;
            } else if line.starts_with("-----BEGIN ") || line.starts_with("-----END ") {
                return Err(TlsBuildError::InvalidPem {
                    owner: owner.into(),
                    kind,
                    path: path.into(),
                    detail: "nested or mismatched PEM boundary",
                });
            }
            continue;
        }

        if line.trim().is_empty() {
            continue;
        }
        let Some(label) = line
            .strip_prefix("-----BEGIN ")
            .and_then(|line| line.strip_suffix("-----"))
        else {
            return Err(TlsBuildError::InvalidPem {
                owner: owner.into(),
                kind,
                path: path.into(),
                detail: "non-PEM data outside a PEM block",
            });
        };
        if label.is_empty() {
            return Err(TlsBuildError::InvalidPem {
                owner: owner.into(),
                kind,
                path: path.into(),
                detail: "empty PEM label",
            });
        }
        open_label = Some(label);
    }

    if open_label.is_some() {
        return Err(TlsBuildError::InvalidPem {
            owner: owner.into(),
            kind,
            path: path.into(),
            detail: "unterminated PEM block",
        });
    }
    if labels.is_empty() {
        return Err(TlsBuildError::InvalidPem {
            owner: owner.into(),
            kind,
            path: path.into(),
            detail: "no PEM blocks",
        });
    }
    Ok(labels)
}
