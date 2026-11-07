#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) fn validate_current(name: &str, leaf: &X509) -> Result<(), TlsBuildError> {
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

pub(super) fn validate_chain(
    name: &str,
    leaf: &X509,
    intermediates: &[X509],
) -> Result<(), TlsBuildError> {
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

pub(super) fn dns_names(
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

pub(super) fn normalize_declared_dns_names(
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

pub(super) fn canonical_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ipv6) => ipv6.to_ipv4_mapped().map_or(IpAddr::V6(ipv6), IpAddr::V4),
        IpAddr::V4(_) => ip,
    }
}

pub(super) fn valid_dns_name(dns_name: &str) -> bool {
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

pub(super) fn preflight(
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

pub(super) fn metadata(
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
