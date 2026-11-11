#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Clone)]
pub(crate) struct CompiledTlsProfilePolicy {
    min_version: TlsVersion,
    alpn: ALPN,
    cipher_list: Option<String>,
    dh_parameters_path: Option<std::path::PathBuf>,
    client_auth: CompiledClientAuthPolicy,
    session_cache: Option<CompiledSessionCachePolicy>,
    session_timeout: Option<i32>,
    session_tickets: bool,
    prefer_server_ciphers: bool,
    ktls: KtlsMode,
    pub(super) selector: CompiledCertificateSelector,
    source_policy: TlsPolicy,
}

#[derive(Clone)]
struct CompiledSessionCachePolicy {
    capacity: i32,
    id_context: [u8; 32],
}

#[derive(Clone)]
pub(super) struct CompiledCertificateSelector {
    pub(super) exact_names: BTreeMap<String, usize>,
    pub(super) wildcard_suffixes: BTreeMap<String, usize>,
}

impl CompiledTlsProfilePolicy {
    #[allow(clippy::too_many_lines)]
    pub(crate) fn compile(
        profile: &TlsProfile,
        identities: &[super::super::TlsIdentityBlueprint],
        certificate_indices: &[usize],
        _default_certificate_index: usize,
    ) -> Result<Self, TlsBuildError> {
        let alpn = compile_alpn(&profile.name, &profile.alpn)?;
        let allowed_dns_names = profile
            .policy
            .client_auth
            .allowed_dns_names
            .iter()
            .map(|dns_name| {
                client_identity(dns_name).ok_or_else(|| TlsBuildError::InvalidTlsClientAuthPolicy {
                    profile: profile.name.clone(),
                    detail: "allowed_dns_names must contain exact DNS names or IP addresses",
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let client_auth = &profile.policy.client_auth;
        match client_auth.mode {
            TlsClientAuthMode::Disabled
                if client_auth.ca_certificate_path.is_some() || !allowed_dns_names.is_empty() =>
            {
                return Err(TlsBuildError::InvalidTlsClientAuthPolicy {
                    profile: profile.name.clone(),
                    detail: "disabled client authentication cannot configure a CA or identities",
                });
            }
            TlsClientAuthMode::Optional | TlsClientAuthMode::Required
                if client_auth.ca_certificate_path.is_none() =>
            {
                return Err(TlsBuildError::InvalidTlsClientAuthPolicy {
                    profile: profile.name.clone(),
                    detail: "enabled client authentication requires a CA certificate path",
                });
            }
            _ => {}
        }
        let session_cache = profile
            .policy
            .session_cache
            .as_ref()
            .map(|cache| {
                Ok(CompiledSessionCachePolicy {
                    capacity: i32::try_from(cache.size_bytes / ESTIMATED_SESSION_BYTES).map_err(
                        |_| TlsBuildError::InvalidTlsProfilePolicy {
                            profile: profile.name.clone(),
                        },
                    )?,
                    id_context: sha256(
                        format!("oxiroute-session-cache:{}:{}", profile.name, cache.name)
                            .as_bytes(),
                    ),
                })
            })
            .transpose()?;
        let session_timeout = profile
            .policy
            .session_timeout_seconds
            .map(|timeout| {
                i32::try_from(timeout).map_err(|_| TlsBuildError::InvalidTlsProfilePolicy {
                    profile: profile.name.clone(),
                })
            })
            .transpose()?;
        let mut exact_names = BTreeMap::new();
        let mut wildcard_suffixes = BTreeMap::new();
        for &index in certificate_indices {
            for dns_name in &identities[index].dns_names {
                if dns_name.parse::<IpAddr>().is_ok() {
                    continue;
                }
                let (names, name) = if let Some(suffix) = dns_name.strip_prefix("*.") {
                    (&mut wildcard_suffixes, suffix)
                } else {
                    (&mut exact_names, dns_name.as_str())
                };
                if let Some(first_index) = names.insert(name.into(), index) {
                    return Err(TlsBuildError::OverlappingProfileDnsName {
                        profile: profile.name.clone(),
                        dns_name: dns_name.clone(),
                        first_certificate: identities[first_index].name.clone(),
                        second_certificate: identities[index].name.clone(),
                    });
                }
            }
        }
        Ok(Self {
            min_version: profile.min_version,
            alpn,
            cipher_list: profile.policy.cipher_list.clone(),
            dh_parameters_path: profile.policy.dh_parameters_path.clone(),
            client_auth: CompiledClientAuthPolicy {
                mode: client_auth.mode,
                ca_certificate_path: client_auth.ca_certificate_path.clone(),
                allowed_dns_names: allowed_dns_names.into(),
            },
            session_cache,
            session_timeout,
            session_tickets: profile.policy.session_tickets,
            prefer_server_ciphers: profile.policy.prefer_server_ciphers,
            ktls: profile.policy.ktls,
            selector: CompiledCertificateSelector {
                exact_names,
                wildcard_suffixes,
            },
            source_policy: profile.policy.clone(),
        })
    }
}

/// Whether connections of the profile are offered the kernel TLS transmit offload. A kernel that cannot do it
/// leaves the profile in user space, and the operator hears it once per generation, here.
fn kernel_tx_available(profile: &str, mode: KtlsMode) -> bool {
    if mode == KtlsMode::Off {
        return false;
    }
    match pingora::protocols::l4::ktls::probe() {
        Ok(()) => {
            log::info!(
                "TLS profile `{profile}`: ktls = \"static\": large static files are sent with kernel TLS"
            );
            true
        }
        Err(unavailable) => {
            log::warn!(
                "TLS profile `{profile}`: ktls = \"static\" is not active, TLS stays in user space: {unavailable}"
            );
            false
        }
    }
}

pub struct TlsProfilePlan {
    name: String,
    min_version: TlsVersion,
    alpn: ALPN,
    policy: TlsPolicy,
    cipher_list: Option<String>,
    dh_parameters: Option<Dh<Params>>,
    client_auth: ClientAuthPlan,
    session_cache: Option<CompiledSessionCachePolicy>,
    session_timeout: Option<i32>,
    session_tickets: bool,
    prefer_server_ciphers: bool,
    kernel_tx: bool,
    tls_alpn_challenge_store: TlsAlpnChallengeStore,
    selector: Arc<CertificateSelector>,
}

impl TlsProfilePlan {
    pub(crate) fn acquire(
        blueprint: &super::super::TlsProfileBlueprint,
        active_identities: &[Arc<ActiveCertificateGeneration>],
        identities: &[super::super::TlsIdentityBlueprint],
        tls_alpn_challenge_store: TlsAlpnChallengeStore,
    ) -> Result<Self, TlsBuildError> {
        let policy = &blueprint.policy;
        let dh_parameters = policy
            .dh_parameters_path
            .as_ref()
            .map(|path| {
                let pem = read_bounded_stable(
                    &blueprint.name,
                    DH_PARAMETERS_FILE,
                    path,
                    MAX_DH_PARAMETERS_BYTES,
                    false,
                )?;
                Dh::params_from_pem(&pem).map_err(|source| TlsBuildError::TlsDhParameters {
                    profile: blueprint.name.clone(),
                    path: path.clone(),
                    source,
                })
            })
            .transpose()?;
        let client_auth = ClientAuthPlan::acquire(&blueprint.name, &policy.client_auth)?;
        let selector = Arc::new(CertificateSelector::acquire(
            blueprint,
            active_identities,
            identities,
        ));
        Ok(Self {
            name: blueprint.name.clone(),
            min_version: policy.min_version,
            alpn: policy.alpn.clone(),
            policy: policy.source_policy.clone(),
            cipher_list: policy.cipher_list.clone(),
            dh_parameters,
            client_auth,
            session_cache: policy.session_cache.clone(),
            session_timeout: policy.session_timeout,
            session_tickets: policy.session_tickets,
            prefer_server_ciphers: policy.prefer_server_ciphers,
            kernel_tx: kernel_tx_available(&blueprint.name, policy.ktls),
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
    pub fn tls_settings(&self, handshake_threads: usize) -> Result<TlsSettings, TlsBuildError> {
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
        if let Some(cipher_list) = &self.cipher_list {
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
        if let Some(compiled) = &self.session_cache {
            settings
                .set_session_id_context(&compiled.id_context)
                .map_err(|source| TlsBuildError::TlsProfileSettings {
                    profile: self.name.clone(),
                    source,
                })?;
            settings.set_session_cache_size(compiled.capacity);
            settings.set_session_cache_mode(SslSessionCacheMode::SERVER);
        } else {
            settings.set_session_cache_mode(SslSessionCacheMode::OFF);
        }
        if let Some(timeout) = self.session_timeout {
            settings.set_session_timeout(timeout);
        }
        if self.session_tickets {
            settings.clear_options(SslOptions::NO_TICKET);
        } else {
            settings.set_options(SslOptions::NO_TICKET);
        }
        // Without a cache or stateless tickets a TLS 1.3 ticket cannot be resumed, so none is sent.
        if !self.session_tickets && self.session_cache.is_none() {
            settings
                .set_num_tickets(0)
                .map_err(|source| TlsBuildError::TlsProfileSettings {
                    profile: self.name.clone(),
                    source,
                })?;
        }
        // Read whatever the socket holds in one call instead of a record header, then its body.
        settings.set_read_ahead(true);
        if self.prefer_server_ciphers {
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
        if handshake_threads > 0 {
            settings.set_offload_threadpool(handshake_threads, 1);
        }
        if self.kernel_tx {
            settings.offer_kernel_tx();
        }
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
