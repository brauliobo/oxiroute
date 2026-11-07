#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) struct ClientAuthPlan {
    mode: TlsClientAuthMode,
    ca_certificates: Arc<[X509]>,
    allowed_dns_names: Arc<[CertificateIdentity]>,
    h3_client_verifier: Option<Arc<dyn rustls::server::danger::ClientCertVerifier>>,
}

#[derive(Clone)]
pub(super) struct CompiledClientAuthPolicy {
    pub(super) mode: TlsClientAuthMode,
    pub(super) ca_certificate_path: Option<std::path::PathBuf>,
    pub(super) allowed_dns_names: Arc<[CertificateIdentity]>,
}

impl ClientAuthPlan {
    pub(super) fn acquire(
        profile: &str,
        policy: &CompiledClientAuthPolicy,
    ) -> Result<Self, TlsBuildError> {
        let (ca_certificates, rustls_roots) = match policy.mode {
            TlsClientAuthMode::Disabled => (
                Arc::from(Vec::<X509>::new().into_boxed_slice()),
                Arc::new(rustls::RootCertStore::empty()),
            ),
            TlsClientAuthMode::Optional | TlsClientAuthMode::Required => load_client_ca_bundle(
                profile,
                policy
                    .ca_certificate_path
                    .as_deref()
                    .expect("compiled client CA path"),
            )?,
        };
        let h3_client_verifier = match policy.mode {
            TlsClientAuthMode::Disabled => None,
            TlsClientAuthMode::Optional | TlsClientAuthMode::Required => {
                let builder = rustls::server::WebPkiClientVerifier::builder(rustls_roots);
                let delegate = match policy.mode {
                    TlsClientAuthMode::Optional => builder.allow_unauthenticated().build(),
                    TlsClientAuthMode::Required => builder.build(),
                    TlsClientAuthMode::Disabled => unreachable!(),
                }
                .map_err(|error| TlsBuildError::ClientCaRustlsVerifier {
                    profile: profile.into(),
                    detail: error.to_string(),
                })?;
                Some(Arc::new(ExactClientCertificateVerifier {
                    delegate,
                    allowed_dns_names: Arc::clone(&policy.allowed_dns_names),
                })
                    as Arc<dyn rustls::server::danger::ClientCertVerifier>)
            }
        };

        Ok(Self {
            mode: policy.mode,
            ca_certificates,
            allowed_dns_names: Arc::clone(&policy.allowed_dns_names),
            h3_client_verifier,
        })
    }

    pub(super) fn apply(
        &self,
        profile: &str,
        settings: &mut TlsSettings,
    ) -> Result<(), TlsBuildError> {
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

    pub(super) fn mode(&self) -> TlsClientAuthMode {
        self.mode
    }

    pub(super) fn ca_configured(&self) -> bool {
        !self.ca_certificates.is_empty()
    }

    pub(super) fn allowed_dns_name_count(&self) -> usize {
        self.allowed_dns_names.len()
    }

    pub(super) fn h3_client_verifier(
        &self,
    ) -> Option<Arc<dyn rustls::server::danger::ClientCertVerifier>> {
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

pub(super) fn client_identity(value: &str) -> Option<CertificateIdentity> {
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
