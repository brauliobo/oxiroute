#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

pub(super) fn load_dns_credentials(
    path: &Path,
    certificate: &str,
) -> Result<Dns01Credentials, AcmeManagedError> {
    let bytes = super::super::read_bounded_stable(
        certificate,
        "DNS-01 credentials",
        path,
        MAX_DNS01_CREDENTIAL_BYTES,
        true,
    )
    .map_err(|source| AcmeManagedError::DnsCredentials(Box::new(source)))?;
    Dns01Credentials::new(bytes.to_vec()).map_err(AcmeManagedError::DnsProvider)
}

pub(super) fn dns_cleanup_path(certificate: &str) -> String {
    format!("certificates/{certificate}/dns-cleanup.json")
}

pub(super) fn persisted_dns_cleanup_status(
    revisions: &RevisionStore,
    certificate: &str,
) -> &'static str {
    match revisions
        .state()
        .read_secret_json::<PersistedDnsCleanup>(&dns_cleanup_path(certificate), MAX_JOB_BYTES)
    {
        Ok(_) => "pending",
        Err(AcmeStateError::FileOpen(error)) if error.kind() == io::ErrorKind::NotFound => "none",
        Err(_) => "failed",
    }
}

pub(super) fn persisted_dns_record(
    certificate: &str,
    journal: &PersistedDnsCleanup,
) -> Result<oxiroute_acme::Dns01Record, Dns01ProviderError> {
    if journal.certificate != certificate {
        return Err(Dns01ProviderError::InvalidRecord);
    }
    let challenge = Dns01Challenge::new(
        &journal.identifier,
        &journal.challenge_url,
        &journal.record_name,
        &journal.record_value,
    )?;
    let record = oxiroute_acme::Dns01Record::new(
        &journal.provider,
        &journal.challenge_url,
        &journal.record_name,
        journal.record_value.as_bytes().to_vec(),
        &journal.provider_record_id,
    )?;
    if !record.matches(&challenge, &journal.provider) {
        return Err(Dns01ProviderError::InvalidRecord);
    }
    Ok(record)
}

pub(super) fn read_persisted_renewal(
    revisions: &RevisionStore,
    certificate: &str,
) -> Option<PersistedRenewal> {
    revisions
        .state()
        .read_json(
            &format!("certificates/{certificate}/renewal.json"),
            MAX_JOB_BYTES,
        )
        .ok()
}

pub(super) fn key_type_string(key_type: AcmeKeyType) -> &'static str {
    match key_type {
        AcmeKeyType::EcdsaP256 => "ecdsa_p256",
        AcmeKeyType::Rsa2048 => "rsa_2048",
    }
}

pub(super) fn challenge_string(challenge: AcmeChallengeType) -> &'static str {
    match challenge {
        AcmeChallengeType::Http01 => "http01",
        AcmeChallengeType::Dns01 => "dns01",
        AcmeChallengeType::TlsAlpn01 => "tls_alpn01",
    }
}

pub(super) fn is_known_error_code(code: &str) -> bool {
    matches!(
        code,
        "job_busy"
            | "state_failed"
            | "poll_timeout"
            | "job_cancelled"
            | "transport_failed"
            | "acme_problem"
            | "protocol_failed"
            | "invalid_candidate"
            | "authorization_failed"
            | "order_failed"
            | "certificate_malformed"
            | "account_directory_changed"
            | "account_not_configured"
            | "revocation_unsupported"
            | "key_change_unsupported"
            | "invalid_certificate"
            | "invalid_revocation_reason"
            | "ip_identifier_unsupported"
            | "renewal_information_invalid"
            | "publication_conflict"
            | "challenge_failed"
            | "dns_provider_unsupported"
            | "dns_credentials_failed"
            | "dns_provider_timeout"
            | "dns_provider_cancelled"
            | "dns_provider_failed"
            | "dns_cleanup_failed"
    )
}

pub(super) fn default_renewal_information_status() -> String {
    "not_advertised".into()
}

pub(super) fn normalized_renewal_information_status(status: &str) -> Option<&'static str> {
    match status {
        "not_advertised" => Some("not_advertised"),
        "pending" => Some("pending"),
        "applied" => Some("applied"),
        "unavailable" => Some("unavailable"),
        "invalid" => Some("invalid"),
        _ => None,
    }
}

pub(super) fn is_cancelled_error(error: &AcmeManagedError) -> bool {
    matches!(
        error,
        AcmeManagedError::Protocol(AcmeError::Cancelled)
            | AcmeManagedError::DnsProvider(Dns01ProviderError::Cancelled)
    )
}

pub(super) fn retry_delay(certificate: &str, attempt: u32) -> u64 {
    let exponent = attempt.saturating_sub(1).min(8);
    let base = ACME_RETRY_BASE_SECONDS.saturating_mul(1_u64 << exponent);
    let base = base.min(ACME_RETRY_MAX_SECONDS);
    let digest = openssl::sha::sha256(format!("{certificate}:{attempt}").as_bytes());
    let jitter = u64::from(digest[0]) % (base / 4 + 1);
    base.saturating_add(jitter).min(ACME_RETRY_MAX_SECONDS)
}

pub(super) fn account_key_algorithm(key_type: AcmeKeyType) -> AccountKeyAlgorithm {
    match key_type {
        AcmeKeyType::EcdsaP256 => AccountKeyAlgorithm::EcdsaP256,
        AcmeKeyType::Rsa2048 => AccountKeyAlgorithm::Rsa2048,
    }
}

pub(super) fn leaf_key_algorithm(key_type: AcmeKeyType) -> LeafKeyAlgorithm {
    match key_type {
        AcmeKeyType::EcdsaP256 => LeafKeyAlgorithm::EcdsaP256,
        AcmeKeyType::Rsa2048 => LeafKeyAlgorithm::Rsa2048,
    }
}

pub(super) fn bootstrap_key_type(key_type: AcmeKeyType) -> SelfSignedKeyType {
    match key_type {
        AcmeKeyType::EcdsaP256 => SelfSignedKeyType::EcdsaP256,
        AcmeKeyType::Rsa2048 => SelfSignedKeyType::Rsa2048,
    }
}

pub(super) fn poll_policy(
    deadline_unix_seconds: u64,
    cancellation: Option<Dns01Cancellation>,
) -> PollPolicy {
    PollPolicy {
        max_attempts: 64,
        deadline_unix_seconds,
        initial_delay_seconds: 1,
        max_delay_seconds: 60,
        cancellation,
    }
}

pub(super) fn asn1_unix(value: &Asn1TimeRef) -> Option<u64> {
    let epoch = Asn1Time::from_unix(0).ok()?;
    let difference = epoch.diff(value).ok()?;
    if difference.days < 0 || difference.secs < 0 {
        return None;
    }
    u64::try_from(difference.days)
        .ok()?
        .checked_mul(86_400)?
        .checked_add(u64::try_from(difference.secs).ok()?)
}

pub(super) fn validate_exact_dns_sans(
    certificate: &X509,
    declared_dns_names: &[String],
) -> Result<(), AcmeManagedError> {
    let expected = declared_dns_names
        .iter()
        .map(|name| name.to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let Some(sans) = certificate.subject_alt_names() else {
        return Err(AcmeManagedError::CertificateMalformed);
    };
    let mut actual = BTreeSet::new();
    for san in sans {
        let Some(dns_name) = san.dnsname() else {
            return Err(AcmeManagedError::CertificateMalformed);
        };
        if !actual.insert(dns_name.to_ascii_lowercase()) {
            return Err(AcmeManagedError::CertificateMalformed);
        }
    }
    if actual != expected {
        return Err(AcmeManagedError::CertificateMalformed);
    }
    Ok(())
}

pub(super) fn issuer_name(certificate: &X509) -> Result<String, AcmeManagedError> {
    let values = certificate
        .issuer_name()
        .entries()
        .map(|entry| {
            entry
                .data()
                .to_string()
                .map_err(|_| AcmeManagedError::CertificateMalformed)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(values.join(","))
}

impl std::fmt::Debug for AcmeManagedReconciler {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcmeManagedReconciler")
            .field("certificate", &self.certificate)
            .field("state_root", &self.revisions.state().root())
            .field(
                "active_revision",
                &self.active.snapshot().metadata().revision,
            )
            .finish_non_exhaustive()
    }
}

pub(super) fn restored_next_action(state: &ReconcileState, certificate: &str) -> Option<u64> {
    if state.auto_retry_blocked {
        return None;
    }
    if state.disk_revision == "bootstrap" {
        return Some(0);
    }
    state
        .retry_at_unix_seconds
        .or(state.suggested_renewal_unix_seconds)
        .or_else(|| {
            state.not_before_unix_seconds.and_then(|not_before| {
                state
                    .not_after_unix_seconds
                    .and_then(|not_after| stable_renewal_time(not_before, not_after, certificate))
            })
        })
}
