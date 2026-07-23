use std::{
    cmp::Ordering,
    collections::HashSet,
    fmt,
    net::IpAddr,
    path::Path,
    sync::{Arc, LazyLock},
};

use openssl::{
    asn1::Asn1Time,
    error::ErrorStack,
    sha::sha256,
    ssl::SslVersion,
    x509::{X509, X509PurposeId, store::X509StoreBuilder, verify::X509VerifyFlags},
};
use oxiroute_config::{HttpVersion, UpstreamPool};
use pingora::{
    protocols::tls::{CaType, TlsConfigureHook, TlsRef},
    upstreams::peer::{HttpPeer, Scheme},
};

use super::{
    MAX_CA_CERTIFICATE_BYTES, TlsBuildError, certificate_is_ca_capable, pem_labels,
    read_bounded_stable,
};

const CA_FILE: &str = "custom CA bundle";
const MAX_CA_CERTIFICATES: usize = 128;
const TLS12_CIPHER_LIST: &str = "ECDHE-ECDSA-AES128-GCM-SHA256:\
    ECDHE-ECDSA-AES256-GCM-SHA384:\
    ECDHE-ECDSA-CHACHA20-POLY1305:\
    ECDHE-RSA-AES128-GCM-SHA256:\
    ECDHE-RSA-AES256-GCM-SHA384:\
    ECDHE-RSA-CHACHA20-POLY1305";
const TLS13_CIPHERSUITES: &str = "TLS_AES_128_GCM_SHA256:\
    TLS_AES_256_GCM_SHA384:\
    TLS_CHACHA20_POLY1305_SHA256";
static SYSTEM_ROOTS_TLS_CONFIGURE_HOOK: LazyLock<TlsConfigureHook> =
    LazyLock::new(|| Arc::new(configure_upstream_tls_with_system_roots));
static CUSTOM_CA_TLS_CONFIGURE_HOOK: LazyLock<TlsConfigureHook> =
    LazyLock::new(|| Arc::new(configure_upstream_tls_with_custom_ca));

struct CustomCaBundle {
    ca: Arc<CaType>,
    revision: String,
    policy: Vec<u8>,
}

/// A compiled, immutable upstream TLS and HTTP negotiation policy.
pub struct UpstreamTlsPlan {
    pool: String,
    server_name: String,
    ca: Option<Arc<CaType>>,
    ca_revision: Option<String>,
    min_http_version: HttpVersion,
    max_http_version: HttpVersion,
    group_key: u64,
}

impl UpstreamTlsPlan {
    /// Prepares the TLS policy for a pool, returning `None` for a plaintext pool.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid SNI/version policy or an unusable custom CA bundle.
    pub fn from_pool(pool: &UpstreamPool) -> Result<Option<Self>, TlsBuildError> {
        let Some(tls) = &pool.tls else {
            return Ok(None);
        };
        let mut server_name = tls.server_name.clone();
        server_name.make_ascii_lowercase();
        if !valid_server_name(&server_name) {
            return Err(TlsBuildError::InvalidUpstreamServerName {
                pool: pool.name.clone(),
                server_name,
            });
        }
        if matches!(
            (pool.http_versions.min, pool.http_versions.max),
            (HttpVersion::Http2, HttpVersion::Http11)
        ) {
            return Err(TlsBuildError::InvalidHttpVersionRange {
                pool: pool.name.clone(),
            });
        }

        let custom_ca = tls
            .ca_certificate_path
            .as_deref()
            .map(|path| load_custom_ca_bundle(&pool.name, path))
            .transpose()?;
        let (ca, ca_revision, ca_policy) = custom_ca.map_or_else(
            || (None, None, vec![0]),
            |bundle| (Some(bundle.ca), Some(bundle.revision), bundle.policy),
        );

        let group_key = group_key(
            &server_name,
            pool.http_versions.min,
            pool.http_versions.max,
            &ca_policy,
        );
        Ok(Some(Self {
            pool: pool.name.clone(),
            server_name,
            ca,
            ca_revision,
            min_http_version: pool.http_versions.min,
            max_http_version: pool.http_versions.max,
            group_key,
        }))
    }

    #[must_use]
    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    #[must_use]
    pub const fn min_http_version(&self) -> HttpVersion {
        self.min_http_version
    }

    #[must_use]
    pub const fn max_http_version(&self) -> HttpVersion {
        self.max_http_version
    }

    #[must_use]
    pub const fn group_key(&self) -> u64 {
        self.group_key
    }

    #[must_use]
    pub const fn uses_custom_ca(&self) -> bool {
        self.ca.is_some()
    }

    /// Applies TLS, strict verification, CA, ALPN, and reuse-isolation fields to a peer.
    pub fn apply_to_peer(&self, peer: &mut HttpPeer) {
        peer.scheme = Scheme::HTTPS;
        peer.sni.clone_from(&self.server_name);
        peer.group_key = self.group_key;
        peer.options.verify_cert = true;
        peer.options.verify_hostname = true;
        peer.options.alternative_cn = None;
        peer.options.ca.clone_from(&self.ca);
        peer.options.upstream_tls_configure_hook = Some(Arc::clone(if self.ca.is_some() {
            &CUSTOM_CA_TLS_CONFIGURE_HOOK
        } else {
            &SYSTEM_ROOTS_TLS_CONFIGURE_HOOK
        }));
        peer.options.set_http_version(
            http_version_number(self.max_http_version),
            http_version_number(self.min_http_version),
        );
    }
}

fn configure_upstream_tls_with_system_roots(ssl: &mut TlsRef) -> Result<(), ErrorStack> {
    configure_upstream_tls(ssl, X509VerifyFlags::X509_STRICT)
}

fn configure_upstream_tls_with_custom_ca(ssl: &mut TlsRef) -> Result<(), ErrorStack> {
    configure_upstream_tls(
        ssl,
        X509VerifyFlags::X509_STRICT | X509VerifyFlags::PARTIAL_CHAIN,
    )
}

fn configure_upstream_tls(
    ssl: &mut TlsRef,
    verify_flags: X509VerifyFlags,
) -> Result<(), ErrorStack> {
    ssl.set_security_level(2);
    ssl.set_min_proto_version(Some(SslVersion::TLS1_2))?;
    ssl.set_cipher_list(TLS12_CIPHER_LIST)?;
    ssl.set_ciphersuites(TLS13_CIPHERSUITES)?;
    ssl.param_mut().set_flags(verify_flags)
}

fn load_custom_ca_bundle(pool: &str, path: &Path) -> Result<CustomCaBundle, TlsBuildError> {
    let pem = read_bounded_stable(pool, CA_FILE, path, MAX_CA_CERTIFICATE_BYTES, false)?;
    let labels = pem_labels(pool, CA_FILE, path, &pem)?;
    if labels.iter().any(|label| *label != "CERTIFICATE") {
        return Err(TlsBuildError::InvalidPem {
            owner: pool.into(),
            kind: CA_FILE,
            path: path.into(),
            detail: "CA bundle may contain only CERTIFICATE blocks",
        });
    }
    if labels.len() > MAX_CA_CERTIFICATES {
        return Err(TlsBuildError::TooManyCaCertificates { pool: pool.into() });
    }
    let certificates = X509::stack_from_pem(&pem).map_err(|source| TlsBuildError::CaParse {
        pool: pool.into(),
        path: path.into(),
        source,
    })?;
    if certificates.len() != labels.len() {
        return Err(TlsBuildError::InvalidPem {
            owner: pool.into(),
            kind: CA_FILE,
            path: path.into(),
            detail: "not every CA certificate PEM block could be parsed",
        });
    }

    let mut policy = vec![1];
    let mut unique_der = HashSet::with_capacity(certificates.len());
    for (index, certificate) in certificates.iter().enumerate() {
        validate_ca_current(pool, index, certificate)?;
        let ca_capable = certificate_is_ca_capable(certificate).map_err(|source| {
            TlsBuildError::CaCertificateInspection {
                pool: pool.into(),
                index,
                source: Box::new(source),
            }
        })?;
        if !ca_capable {
            return Err(TlsBuildError::NonCaCertificate {
                pool: pool.into(),
                index,
            });
        }
        let der = certificate
            .to_der()
            .map_err(|source| TlsBuildError::CaPolicy {
                pool: pool.into(),
                source,
            })?;
        if !unique_der.insert(der.clone()) {
            return Err(TlsBuildError::DuplicateCaCertificate {
                pool: pool.into(),
                index,
            });
        }
        policy.extend_from_slice(&(der.len() as u64).to_be_bytes());
        policy.extend_from_slice(&der);
    }
    preflight_ca_store(pool, &certificates)?;
    Ok(CustomCaBundle {
        ca: Arc::new(certificates.into_boxed_slice()),
        revision: lower_hex(&sha256(&policy)),
        policy,
    })
}

fn validate_ca_current(pool: &str, index: usize, certificate: &X509) -> Result<(), TlsBuildError> {
    let now =
        Asn1Time::days_from_now(0).map_err(|source| TlsBuildError::CaCertificateInspection {
            pool: pool.into(),
            index,
            source: Box::new(source),
        })?;
    let not_before = certificate.not_before().compare(&now).map_err(|source| {
        TlsBuildError::CaCertificateInspection {
            pool: pool.into(),
            index,
            source: Box::new(source),
        }
    })?;
    if not_before == Ordering::Greater {
        return Err(TlsBuildError::CaCertificateNotYetValid {
            pool: pool.into(),
            index,
            not_before: certificate.not_before().to_string(),
        });
    }
    let not_after = certificate.not_after().compare(&now).map_err(|source| {
        TlsBuildError::CaCertificateInspection {
            pool: pool.into(),
            index,
            source: Box::new(source),
        }
    })?;
    if not_after != Ordering::Greater {
        return Err(TlsBuildError::CaCertificateExpired {
            pool: pool.into(),
            index,
            not_after: certificate.not_after().to_string(),
        });
    }
    Ok(())
}

fn preflight_ca_store(pool: &str, certificates: &[X509]) -> Result<(), TlsBuildError> {
    let store_error = |source| TlsBuildError::CaStore {
        pool: pool.into(),
        source: Box::new(source),
    };
    let mut store = X509StoreBuilder::new().map_err(store_error)?;
    store
        .set_flags(X509VerifyFlags::X509_STRICT | X509VerifyFlags::PARTIAL_CHAIN)
        .map_err(store_error)?;
    store
        .set_purpose(X509PurposeId::SSL_SERVER)
        .map_err(store_error)?;
    for certificate in certificates {
        store.add_cert(certificate.clone()).map_err(store_error)?;
    }
    let _store = store.build();
    Ok(())
}

impl fmt::Debug for UpstreamTlsPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("UpstreamTlsPlan")
            .field("pool", &self.pool)
            .field("server_name", &self.server_name)
            .field("ca_revision", &self.ca_revision)
            .field("min_http_version", &self.min_http_version)
            .field("max_http_version", &self.max_http_version)
            .field("group_key", &self.group_key)
            .finish_non_exhaustive()
    }
}

/// Prepares an upstream policy for later application to each selected `HttpPeer`.
///
/// # Errors
///
/// Returns an error when the pool's TLS policy cannot be safely compiled.
pub fn prepare_upstream_tls(pool: &UpstreamPool) -> Result<Option<UpstreamTlsPlan>, TlsBuildError> {
    UpstreamTlsPlan::from_pool(pool)
}

fn valid_server_name(server_name: &str) -> bool {
    server_name.is_ascii()
        && !server_name.is_empty()
        && server_name.len() <= 253
        && !server_name.ends_with('.')
        && !server_name.contains('*')
        && server_name.parse::<IpAddr>().is_err()
        && server_name.split('.').all(|label| {
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

const fn http_version_number(version: HttpVersion) -> u8 {
    match version {
        HttpVersion::Http11 => 1,
        HttpVersion::Http2 => 2,
    }
}

fn group_key(
    server_name: &str,
    min_http_version: HttpVersion,
    max_http_version: HttpVersion,
    ca_policy: &[u8],
) -> u64 {
    let mut policy = b"oxiroute-upstream-tls-v2\0".to_vec();
    policy.extend_from_slice(server_name.as_bytes());
    policy.push(0);
    policy.push(http_version_number(min_http_version));
    policy.push(http_version_number(max_http_version));
    policy.extend_from_slice(ca_policy);
    let digest = sha256(&policy);
    let key = u64::from_be_bytes(digest[..8].try_into().expect("SHA-256 has eight bytes"));
    key.max(1)
}

fn lower_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}
