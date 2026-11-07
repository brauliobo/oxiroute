#[allow(clippy::wildcard_imports)]
use super::*;

const CERTIFICATE_FILE: &str = "certificate chain";
const PRIVATE_KEY_FILE: &str = "private key";
pub(super) const DH_PARAMETERS_FILE: &str = "DH parameters";
pub(super) const ESTIMATED_SESSION_BYTES: u64 = 256;
pub(super) const MAX_DNS_NAMES: usize = 100;
const MAX_SELF_SIGNED_VALIDITY_DAYS: u32 = 30;
pub(super) const MAX_CERTIFICATE_PUBLICATION_ATTEMPTS: usize = 4;

#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(super) enum CertificateIdentity {
    Dns(String),
    Ip(IpAddr),
}

impl CertificateIdentity {
    pub(super) fn into_string(self) -> String {
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
    pub(super) metadata: CertificateMetadata,
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
    pub(in super::super) fn self_signed_development(
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
