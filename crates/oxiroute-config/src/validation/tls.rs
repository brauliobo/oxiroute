#[allow(clippy::wildcard_imports)]
use super::*;

#[allow(clippy::too_many_lines)]
pub(super) fn validate_certificates(certificates: &mut [Certificate]) -> Result<(), ConfigError> {
    validate_names(
        "certificate",
        certificates
            .iter()
            .map(|certificate| certificate.name.as_str()),
    )?;
    if certificates.len() > MAX_CERTIFICATES {
        return Err(ConfigError::TooManyCertificates);
    }

    for certificate in certificates {
        if certificate.dns_names.is_empty() {
            return Err(ConfigError::EmptyCertificateDnsNames {
                certificate: certificate.name.clone(),
            });
        }
        if certificate.dns_names.len() > MAX_CERTIFICATE_DNS_NAMES {
            return Err(ConfigError::TooManyCertificateDnsNames {
                certificate: certificate.name.clone(),
            });
        }
        let mut unique_dns_names = HashSet::with_capacity(certificate.dns_names.len());
        for dns_name in &mut certificate.dns_names {
            if let Ok(ip) = dns_name.parse::<IpAddr>() {
                *dns_name = canonical_ip(ip).to_string();
            } else {
                dns_name.make_ascii_lowercase();
            }
            if !is_valid_certificate_dns_name(dns_name) {
                return Err(ConfigError::InvalidCertificateDnsName {
                    certificate: certificate.name.clone(),
                    dns_name: dns_name.clone(),
                });
            }
            if !unique_dns_names.insert(dns_name.clone()) {
                return Err(ConfigError::DuplicateCertificateDnsName {
                    certificate: certificate.name.clone(),
                    dns_name: dns_name.clone(),
                });
            }
        }

        match &certificate.source {
            CertificateSource::Files {
                certificate_chain_path,
                private_key_path,
            } => {
                validate_file_path(
                    "certificate",
                    &certificate.name,
                    "source.certificate_chain_path",
                    certificate_chain_path,
                )?;
                validate_file_path(
                    "certificate",
                    &certificate.name,
                    "source.private_key_path",
                    private_key_path,
                )?;
                if certificate_chain_path == private_key_path {
                    return Err(ConfigError::DuplicateCertificatePaths {
                        certificate: certificate.name.clone(),
                    });
                }
            }
            CertificateSource::Certbot {
                live_directory_path,
                archive_directory_path,
            } => {
                validate_directory_path(
                    "certificate",
                    &certificate.name,
                    "source.live_directory_path",
                    live_directory_path,
                )?;
                validate_directory_path(
                    "certificate",
                    &certificate.name,
                    "source.archive_directory_path",
                    archive_directory_path,
                )?;
                if live_directory_path == archive_directory_path {
                    return Err(ConfigError::DuplicateCertbotDirectories {
                        certificate: certificate.name.clone(),
                    });
                }
            }
            CertificateSource::AcmeManaged {
                directory_url,
                state_root,
                contacts,
                terms_agreed,
                challenge,
                allowed_dns_suffixes,
                dns01,
                retained_revisions,
                retention_days,
                ..
            } => {
                validate_acme_source(
                    certificate,
                    directory_url,
                    state_root,
                    contacts,
                    *terms_agreed,
                    *challenge,
                    allowed_dns_suffixes,
                    dns01.as_ref(),
                    *retained_revisions,
                    *retention_days,
                )?;
            }
            CertificateSource::SelfSignedDevelopment { validity_days, .. } => {
                if !(MIN_SELF_SIGNED_VALIDITY_DAYS..=MAX_SELF_SIGNED_VALIDITY_DAYS)
                    .contains(validity_days)
                {
                    return Err(ConfigError::InvalidSelfSignedValidityDays {
                        certificate: certificate.name.clone(),
                        value: *validity_days,
                        min: MIN_SELF_SIGNED_VALIDITY_DAYS,
                        max: MAX_SELF_SIGNED_VALIDITY_DAYS,
                    });
                }
            }
        }
    }

    Ok(())
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn validate_acme_source(
    certificate: &Certificate,
    directory_url: &str,
    state_root: &Path,
    contacts: &[String],
    terms_agreed: bool,
    challenge: crate::model::AcmeChallengeType,
    allowed_dns_suffixes: &[String],
    dns01: Option<&AcmeDns01Config>,
    retained_revisions: u32,
    retention_days: u32,
) -> Result<(), ConfigError> {
    if !certificate
        .name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || certificate.name == "."
        || certificate.name == ".."
        || certificate.name.len() > 128
    {
        return Err(ConfigError::InvalidAcmeCertificateName {
            certificate: certificate.name.clone(),
        });
    }
    let parsed_directory_url = directory_url.parse::<Uri>().ok();
    if directory_url.len() > MAX_ACME_DIRECTORY_URL_BYTES
        || !directory_url.is_ascii()
        || !directory_url.starts_with("https://")
        || directory_url.contains('@')
        || directory_url.contains('#')
        || parsed_directory_url.as_ref().is_none_or(|url| {
            url.scheme_str() != Some("https")
                || url
                    .authority()
                    .is_none_or(|authority| authority.host().is_empty())
        })
    {
        return Err(ConfigError::InvalidAcmeDirectoryUrl {
            certificate: certificate.name.clone(),
        });
    }
    validate_directory_path(
        "certificate",
        &certificate.name,
        "source.state_root",
        state_root,
    )?;
    if !terms_agreed {
        return Err(ConfigError::AcmeTermsNotAgreed {
            certificate: certificate.name.clone(),
        });
    }
    match challenge {
        crate::model::AcmeChallengeType::Http01 | crate::model::AcmeChallengeType::TlsAlpn01 => {
            if dns01.is_some() {
                return Err(ConfigError::InvalidAcmeDns01Provider {
                    certificate: certificate.name.clone(),
                });
            }
        }
        crate::model::AcmeChallengeType::Dns01 => {
            let Some(dns01) = dns01 else {
                return Err(ConfigError::InvalidAcmeDns01Credentials {
                    certificate: certificate.name.clone(),
                });
            };
            validate_acme_dns01_source(certificate, dns01)?;
        }
    }
    if contacts.len() > MAX_ACME_CONTACTS
        || contacts.iter().any(|contact| {
            contact.is_empty()
                || contact.len() > 320
                || !contact.is_ascii()
                || !contact.starts_with("mailto:")
        })
    {
        return Err(ConfigError::InvalidAcmeContacts {
            certificate: certificate.name.clone(),
        });
    }
    if allowed_dns_suffixes.is_empty() || allowed_dns_suffixes.len() > MAX_ACME_DNS_SUFFIXES {
        return Err(ConfigError::InvalidAcmeDnsSuffixes {
            certificate: certificate.name.clone(),
        });
    }
    if retained_revisions == 0
        || retained_revisions > MAX_ACME_RETAINED_REVISIONS
        || retention_days == 0
        || retention_days > MAX_ACME_RETENTION_DAYS
    {
        return Err(ConfigError::InvalidAcmeRetention {
            certificate: certificate.name.clone(),
        });
    }
    let mut suffixes = HashSet::with_capacity(allowed_dns_suffixes.len());
    for suffix in allowed_dns_suffixes {
        let suffix = suffix.trim().to_ascii_lowercase();
        if suffix.is_empty()
            || suffix.starts_with("*.")
            || suffix.parse::<IpAddr>().is_ok()
            || !is_valid_certificate_dns_name(&suffix)
            || !suffixes.insert(suffix)
        {
            return Err(ConfigError::InvalidAcmeDnsSuffixes {
                certificate: certificate.name.clone(),
            });
        }
    }
    for dns_name in &certificate.dns_names {
        if dns_name.parse::<IpAddr>().is_ok() {
            return Err(ConfigError::AcmeIdentifierUnsupported {
                certificate: certificate.name.clone(),
            });
        }
        if dns_name.starts_with("*.")
            && !matches!(challenge, crate::model::AcmeChallengeType::Dns01)
        {
            return Err(ConfigError::AcmeWildcardRequiresDns01 {
                certificate: certificate.name.clone(),
                dns_name: dns_name.clone(),
            });
        }
        let policy_name = dns_name.strip_prefix("*.").unwrap_or(dns_name);
        if !suffixes
            .iter()
            .any(|suffix| policy_name == suffix || policy_name.ends_with(&format!(".{suffix}")))
        {
            return Err(ConfigError::AcmeIdentifierOutsidePolicy {
                certificate: certificate.name.clone(),
                dns_name: dns_name.clone(),
            });
        }
    }
    Ok(())
}

fn validate_acme_dns01_source(
    certificate: &Certificate,
    dns01: &AcmeDns01Config,
) -> Result<(), ConfigError> {
    if dns01.provider.is_empty()
        || dns01.provider.len() > MAX_ACME_DNS01_PROVIDER_BYTES
        || !dns01
            .provider
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        || dns01.provider.starts_with('.')
        || dns01.provider.ends_with('.')
        || dns01.provider.eq_ignore_ascii_case("shell")
        || dns01.provider.eq_ignore_ascii_case("exec")
        || dns01.provider.eq_ignore_ascii_case("dynamic")
    {
        return Err(ConfigError::InvalidAcmeDns01Provider {
            certificate: certificate.name.clone(),
        });
    }
    if dns01.timeout_seconds == 0 || dns01.timeout_seconds > MAX_ACME_DNS01_TIMEOUT_SECONDS {
        return Err(ConfigError::InvalidAcmeDns01Timeout {
            certificate: certificate.name.clone(),
        });
    }
    validate_file_path(
        "certificate",
        &certificate.name,
        "source.dns01.credential_file",
        &dns01.credential_file,
    )
    .map_err(|_| ConfigError::InvalidAcmeDns01Credentials {
        certificate: certificate.name.clone(),
    })
}

pub(super) fn validate_tls_profiles(
    tls_profiles: &mut [TlsProfile],
    certificates: &[Certificate],
) -> Result<(), ConfigError> {
    validate_names(
        "TLS profile",
        tls_profiles.iter().map(|profile| profile.name.as_str()),
    )?;
    if tls_profiles.len() > MAX_TLS_PROFILES {
        return Err(ConfigError::TooManyTlsProfiles);
    }

    let certificates_by_name = certificates
        .iter()
        .map(|certificate| (certificate.name.as_str(), certificate))
        .collect::<HashMap<_, _>>();
    for profile in tls_profiles {
        if profile.certificates.is_empty() {
            return Err(ConfigError::EmptyTlsProfileCertificates {
                profile: profile.name.clone(),
            });
        }

        let mut referenced_certificates = HashSet::with_capacity(profile.certificates.len());
        let mut dns_name_owners = HashMap::new();
        for certificate_name in &profile.certificates {
            if !referenced_certificates.insert(certificate_name.as_str()) {
                return Err(ConfigError::DuplicateTlsProfileCertificate {
                    profile: profile.name.clone(),
                    certificate: certificate_name.clone(),
                });
            }
            let certificate = certificates_by_name
                .get(certificate_name.as_str())
                .ok_or_else(|| ConfigError::UnknownTlsProfileCertificate {
                    profile: profile.name.clone(),
                    certificate: certificate_name.clone(),
                })?;
            for dns_name in &certificate.dns_names {
                if dns_name.parse::<IpAddr>().is_ok() {
                    continue;
                }
                if let Some(first_certificate) =
                    dns_name_owners.insert(dns_name.as_str(), certificate.name.as_str())
                {
                    return Err(ConfigError::OverlappingTlsProfileDnsName {
                        profile: profile.name.clone(),
                        dns_name: dns_name.clone(),
                        first_certificate: first_certificate.into(),
                        second_certificate: certificate.name.clone(),
                    });
                }
            }
        }
        if !referenced_certificates.contains(profile.default_certificate.as_str()) {
            return Err(ConfigError::TlsProfileDefaultNotListed {
                profile: profile.name.clone(),
                certificate: profile.default_certificate.clone(),
            });
        }
        if !matches!(
            profile.alpn.as_slice(),
            [AlpnProtocol::Http11 | AlpnProtocol::H2 | AlpnProtocol::H3]
                | [AlpnProtocol::H2, AlpnProtocol::Http11]
        ) {
            return Err(ConfigError::InvalidTlsProfileAlpn {
                profile: profile.name.clone(),
            });
        }
        validate_tls_policy(profile)?;
    }

    Ok(())
}

#[allow(clippy::too_many_lines)]
fn validate_tls_policy(profile: &mut TlsProfile) -> Result<(), ConfigError> {
    let client_auth = &profile.policy.client_auth;
    match client_auth.mode {
        crate::model::TlsClientAuthMode::Disabled => {
            if client_auth.ca_certificate_path.is_some() {
                return Err(ConfigError::InvalidTlsProfilePolicy {
                    profile: profile.name.clone(),
                    field: "policy.client_auth.ca_certificate_path",
                    detail: "must be omitted when client authentication is disabled",
                });
            }
            if !client_auth.allowed_dns_names.is_empty() {
                return Err(ConfigError::InvalidTlsProfilePolicy {
                    profile: profile.name.clone(),
                    field: "policy.client_auth.allowed_dns_names",
                    detail: "must be empty when client authentication is disabled",
                });
            }
        }
        crate::model::TlsClientAuthMode::Optional | crate::model::TlsClientAuthMode::Required => {
            let Some(path) = &client_auth.ca_certificate_path else {
                return Err(ConfigError::InvalidTlsProfilePolicy {
                    profile: profile.name.clone(),
                    field: "policy.client_auth.ca_certificate_path",
                    detail: "is required when client authentication is enabled",
                });
            };
            validate_file_path(
                "TLS profile",
                &profile.name,
                "policy.client_auth.ca_certificate_path",
                path,
            )?;
        }
    }
    if client_auth.allowed_dns_names.len() > MAX_CERTIFICATE_DNS_NAMES {
        return Err(ConfigError::TooManyTlsClientAuthDnsNames {
            profile: profile.name.clone(),
        });
    }

    let mut allowed_dns_names = Vec::with_capacity(client_auth.allowed_dns_names.len());
    let mut unique_allowed_dns_names = HashSet::with_capacity(client_auth.allowed_dns_names.len());
    for dns_name in &client_auth.allowed_dns_names {
        let mut normalized = dns_name.clone();
        if let Ok(ip) = normalized.parse::<IpAddr>() {
            normalized = canonical_ip(ip).to_string();
        } else {
            normalized.make_ascii_lowercase();
        }
        if normalized.starts_with("*.") || !is_valid_certificate_dns_name(&normalized) {
            return Err(ConfigError::InvalidTlsClientAuthDnsName {
                profile: profile.name.clone(),
                dns_name: dns_name.clone(),
            });
        }
        if !unique_allowed_dns_names.insert(normalized.clone()) {
            return Err(ConfigError::DuplicateTlsClientAuthDnsName {
                profile: profile.name.clone(),
                dns_name: normalized,
            });
        }
        allowed_dns_names.push(normalized);
    }
    profile.policy.client_auth.allowed_dns_names = allowed_dns_names;

    if profile
        .policy
        .cipher_list
        .as_ref()
        .is_some_and(|value| value.is_empty() || value.as_bytes().contains(&0))
    {
        return Err(ConfigError::InvalidTlsProfilePolicy {
            profile: profile.name.clone(),
            field: "cipher_list",
            detail: "must be nonempty and contain no NUL bytes",
        });
    }
    if let Some(path) = &profile.policy.dh_parameters_path {
        validate_file_path(
            "TLS profile",
            &profile.name,
            "policy.dh_parameters_path",
            path,
        )?;
    }
    if let Some(cache) = &profile.policy.session_cache {
        if cache.name.is_empty()
            || cache.name.len() > 255
            || !cache
                .name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
        {
            return Err(ConfigError::InvalidTlsProfilePolicy {
                profile: profile.name.clone(),
                field: "session_cache.name",
                detail: "must be 1 through 255 ASCII letters, digits, `_`, `-`, or `.`",
            });
        }
        if !(256..=u64::from(i32::MAX as u32) * 256).contains(&cache.size_bytes) {
            return Err(ConfigError::InvalidTlsProfilePolicy {
                profile: profile.name.clone(),
                field: "session_cache.size_bytes",
                detail: "must hold between 1 and i32::MAX estimated 256-byte sessions",
            });
        }
    }
    if profile
        .policy
        .session_timeout_seconds
        .is_some_and(|seconds| seconds == 0 || seconds > u64::from(i32::MAX as u32))
    {
        return Err(ConfigError::InvalidTlsProfilePolicy {
            profile: profile.name.clone(),
            field: "session_timeout_seconds",
            detail: "must be between 1 and i32::MAX seconds",
        });
    }
    Ok(())
}
