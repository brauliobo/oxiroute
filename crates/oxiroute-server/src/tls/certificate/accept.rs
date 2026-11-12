#[allow(clippy::wildcard_imports)]
use super::*;
use std::borrow::Cow;

pub(super) struct CertificateSelector {
    pub(super) default_certificate: String,
    pub(super) active_generations: BTreeMap<String, Arc<ActiveCertificateGeneration>>,
    exact_names: BTreeMap<String, String>,
    wildcard_suffixes: BTreeMap<String, String>,
}

impl CertificateSelector {
    pub(super) fn acquire(
        blueprint: &super::super::TlsProfileBlueprint,
        active_identities: &[Arc<ActiveCertificateGeneration>],
        identities: &[super::super::TlsIdentityBlueprint],
    ) -> Self {
        let active_generations = blueprint
            .certificate_indices
            .iter()
            .map(|&index| {
                (
                    identities[index].name.clone(),
                    Arc::clone(&active_identities[index]),
                )
            })
            .collect();
        let exact_names = blueprint
            .policy
            .selector
            .exact_names
            .iter()
            .map(|(name, &index)| (name.clone(), identities[index].name.clone()))
            .collect();
        let wildcard_suffixes = blueprint
            .policy
            .selector
            .wildcard_suffixes
            .iter()
            .map(|(name, &index)| (name.clone(), identities[index].name.clone()))
            .collect();
        Self {
            default_certificate: identities[blueprint.default_certificate_index].name.clone(),
            active_generations,
            exact_names,
            wildcard_suffixes,
        }
    }

    pub(super) fn select(&self, server_name: Option<&str>) -> &Arc<ActiveCertificateGeneration> {
        let certificate_name = server_name
            .and_then(normalize_server_name)
            .and_then(|name| {
                self.exact_names.get(name.as_ref()).or_else(|| {
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

/// The name a `ClientHello`'s SNI selects certificates by: valid, not a wildcard, lower case. Names are already lower case
/// in practice, so only a name with capitals is copied.
fn normalize_server_name(server_name: &str) -> Option<Cow<'_, str>> {
    if !valid_dns_name(server_name) || server_name.starts_with("*.") {
        return None;
    }
    Some(
        if server_name.bytes().any(|byte| byte.is_ascii_uppercase()) {
            Cow::Owned(server_name.to_ascii_lowercase())
        } else {
            Cow::Borrowed(server_name)
        },
    )
}

pub(super) struct GenerationTlsAccept {
    pub(super) profile: String,
    pub(super) selector: Arc<CertificateSelector>,
}

pub(super) enum TlsAlpnSelection {
    Challenge(Arc<TlsAlpnChallengeIdentity>),
}

static TLS_ALPN_SELECTION_INDEX: OnceLock<
    Result<openssl::ex_data::Index<PingoraSsl, TlsAlpnSelection>, String>,
> = OnceLock::new();

pub(super) fn tls_alpn_selection_index()
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
            if !challenge.usable(super::super::tls_alpn::unix_now()) {
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

pub(super) fn select_alpn<'a>(
    ssl: &mut TlsRef,
    offered: &'a [u8],
    normal_alpn: &ALPN,
    challenge_store: &TlsAlpnChallengeStore,
    selection_index: openssl::ex_data::Index<PingoraSsl, TlsAlpnSelection>,
) -> Result<&'a [u8], AlpnError> {
    let challenge_protocol = find_offered_protocol(offered, super::super::TLS_ALPN_PROTOCOL)?;
    if let Some(challenge_protocol) = challenge_protocol {
        let Some(server_name) = ssl.servername(NameType::HOST_NAME) else {
            return Err(AlpnError::ALERT_FATAL);
        };
        let Some(challenge) =
            challenge_store.lookup(server_name, super::super::tls_alpn::unix_now())
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

pub(super) fn compile_alpn(
    profile: &str,
    protocols: &[AlpnProtocol],
) -> Result<ALPN, TlsBuildError> {
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
