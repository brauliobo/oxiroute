#[allow(clippy::wildcard_imports)]
use super::*;

pub const MAX_CERTIFICATE_CHAIN_BYTES: usize = 1024 * 1024;
pub const MAX_PRIVATE_KEY_BYTES: usize = 256 * 1024;
pub const MAX_DH_PARAMETERS_BYTES: usize = 64 * 1024;
pub const MAX_CA_CERTIFICATE_BYTES: usize = 1024 * 1024;
pub const MAX_CERTIFICATES_IN_CHAIN: usize = 16;
pub const MAX_CLIENT_CA_CERTIFICATES: usize = 128;

pub type CertificateIdentityMap = BTreeMap<String, Arc<ActiveCertificateGeneration>>;
pub type TlsProfilePlanMap = BTreeMap<String, Arc<TlsProfilePlan>>;

pub(crate) struct TlsBlueprint {
    pub(crate) identities: Box<[TlsIdentityBlueprint]>,
    pub(crate) profiles: Box<[TlsProfileBlueprint]>,
}

#[derive(Clone)]
pub(crate) struct TlsIdentityBlueprint {
    pub(crate) name: String,
    pub(crate) dns_names: Box<[String]>,
    pub(crate) source: oxiroute_config::CertificateSource,
}

#[derive(Clone)]
pub(crate) struct TlsProfileBlueprint {
    pub(crate) name: String,
    pub(crate) certificate_indices: Box<[usize]>,
    pub(crate) default_certificate_index: usize,
    pub(crate) policy: CompiledTlsProfilePolicy,
}

impl TlsBlueprint {
    pub(crate) fn compile(config: &oxiroute_config::ConfigDraft) -> Result<Self, TlsBuildError> {
        let identities = config
            .certificates
            .iter()
            .map(|certificate| TlsIdentityBlueprint {
                name: certificate.name.clone(),
                dns_names: certificate.dns_names.clone().into_boxed_slice(),
                source: certificate.source.clone(),
            })
            .collect::<Box<[_]>>();
        let profiles = config
            .tls_profiles
            .iter()
            .map(|profile| {
                let certificate_indices = profile
                    .certificates
                    .iter()
                    .map(|name| {
                        config
                            .certificates
                            .iter()
                            .position(|certificate| certificate.name == *name)
                            .expect("validated TLS certificate reference")
                    })
                    .collect::<Box<[_]>>();
                let default_certificate_index = config
                    .certificates
                    .iter()
                    .position(|certificate| certificate.name == profile.default_certificate)
                    .expect("validated default TLS certificate reference");
                let policy = CompiledTlsProfilePolicy::compile(
                    profile,
                    &identities,
                    &certificate_indices,
                    default_certificate_index,
                )?;
                Ok(TlsProfileBlueprint {
                    name: profile.name.clone(),
                    certificate_indices,
                    default_certificate_index,
                    policy,
                })
            })
            .collect::<Result<Box<[_]>, TlsBuildError>>()?;
        Ok(Self {
            identities,
            profiles,
        })
    }
}

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
/// ```compile_fail
/// use oxiroute_config::ConfigDraft;
///
/// let draft: ConfigDraft = todo!();
/// let _ = oxiroute_server::tls::prepare_tls(&draft);
/// ```
pub fn prepare_tls(config: &ValidatedConfig) -> Result<PreparedTls, TlsBuildError> {
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
    config: &ValidatedConfig,
    dns01_providers: Dns01ProviderRegistry,
) -> Result<PreparedTls, TlsBuildError> {
    let blueprint = TlsBlueprint::compile(config.as_draft())?;
    prepare_tls_blueprint_with_dns01_providers(&blueprint, dns01_providers)
}

pub(crate) fn prepare_tls_blueprint(
    blueprint: &TlsBlueprint,
) -> Result<PreparedTls, TlsBuildError> {
    prepare_tls_blueprint_with_dns01_providers(blueprint, Dns01ProviderRegistry::default())
}

#[allow(clippy::needless_pass_by_value, clippy::too_many_lines)]
fn prepare_tls_blueprint_with_dns01_providers(
    blueprint: &TlsBlueprint,
    dns01_providers: Dns01ProviderRegistry,
) -> Result<PreparedTls, TlsBuildError> {
    let mut active_identities = Vec::with_capacity(blueprint.identities.len());
    let mut acme_reconcilers = Vec::new();
    let mut acme_states = BTreeMap::new();
    let challenge_store = ChallengeStore::default();
    let tls_alpn_challenge_store = tls_alpn::TlsAlpnChallengeStore::default();
    let mut certbot_reconcilers = Vec::new();
    let mut file_reconcilers = Vec::new();
    for certificate in &blueprint.identities {
        let name = certificate.name.clone();
        let (generation, certbot, direct_files, managed_state, managed_policy, dns_provider) =
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
        active_identities.push(Arc::clone(&active));
        if let Some((lineage, archive_revision)) = certbot {
            certbot_reconcilers.push(Arc::new(CertbotReconciler::new(
                lineage,
                name,
                certificate.dns_names.to_vec(),
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
                certificate.dns_names.to_vec(),
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
                certificate.dns_names.to_vec(),
                certificate_chain_path,
                private_key_path,
                active,
            )));
        }
    }

    let profiles = blueprint
        .profiles
        .iter()
        .map(|profile| {
            let plan = Arc::new(TlsProfilePlan::acquire(
                profile,
                &active_identities,
                &blueprint.identities,
                tls_alpn_challenge_store.clone(),
            )?);
            Ok((profile.name.clone(), plan))
        })
        .collect::<Result<BTreeMap<_, _>, TlsBuildError>>()?;
    let certificates = blueprint
        .identities
        .iter()
        .zip(active_identities)
        .map(|(identity, active)| (identity.name.clone(), active))
        .collect();

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
