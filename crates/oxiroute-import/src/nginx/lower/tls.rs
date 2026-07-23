use std::{
    collections::{HashMap, HashSet},
    fs::File,
    io::Read,
    net::IpAddr,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use openssl::{
    pkey::{Id, PKey},
    x509::X509,
};
use oxiroute_config::{AlpnProtocol, Certificate, CertificateSource, TlsProfile, TlsVersion};
use rustix::fs::{self as rustix_fs, Mode, OFlags};
use x509_parser::{extensions::GeneralName, parse_x509_certificate, pem::Pem};
use zeroize::Zeroizing;

use crate::{E_INVALID_VALUE, E_SEMANTICS_NOT_REPRESENTABLE};

use crate::nginx::{
    DirectiveOrigin, EffectiveBind, EffectiveHttp, EffectiveServer, ListenEndpoint, OccurrenceId,
    ServerNameKind,
};

use super::{
    LowerIssue, Lowerer,
    listener::{
        canonical_dns_name, canonical_exact_host, canonical_wildcard_host, matching_listen,
    },
    provenance::{issue, utf8},
};

const MAX_CERTIFICATE_CHAIN_BYTES: usize = 4 * 1024 * 1024;
const MAX_PRIVATE_KEY_BYTES: usize = 256 * 1024;
const MAX_CERTIFICATES_IN_CHAIN: usize = 16;
const MAX_CERTIFICATE_DNS_NAMES: usize = 100;

struct CertificateCandidate {
    name: String,
    dns_names: Vec<String>,
    chain: PathBuf,
    key: PathBuf,
    origin: DirectiveOrigin,
}

pub(super) struct LoweredTls {
    pub(super) certificates: Vec<Certificate>,
    pub(super) profile: TlsProfile,
    pub(super) origins: Vec<DirectiveOrigin>,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub(super) struct CertificateIdentity {
    chain: PathBuf,
    key: PathBuf,
}

#[derive(Clone)]
pub(super) struct CertificateMetadata {
    name: String,
    dns_names: Vec<String>,
}

struct TlsListenPolicy<'a> {
    server: &'a EffectiveServer,
    tls: bool,
    h2: bool,
}

#[derive(Clone, Copy)]
enum MaterialFailure {
    Certificate,
    PrivateKey,
}

#[derive(Eq, PartialEq)]
struct CertificateFingerprint {
    length: u64,
    device: u64,
    inode: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
    changed_seconds: i64,
    changed_nanoseconds: i64,
}

fn canonical_file_path(value: &[u8]) -> Option<PathBuf> {
    let value = utf8(value)?;
    let path = Path::new(value);
    (value.len() <= 4096
        && path.is_absolute()
        && !value.as_bytes().contains(&0)
        && !value.contains("//")
        && !value.ends_with('/')
        && !value
            .split('/')
            .any(|segment| segment == "." || segment == ".."))
    .then(|| path.to_path_buf())
}

fn load_certificate_identity(
    certificate_path: &Path,
    private_key_path: &Path,
) -> Result<Vec<String>, MaterialFailure> {
    let (dns_names, leaf) = read_certificate(certificate_path)?;
    let private_key_bytes = read_private_key(private_key_path)?;
    if !has_one_supported_private_key_pem(&private_key_bytes) {
        return Err(MaterialFailure::PrivateKey);
    }
    let private_key =
        PKey::private_key_from_pem(&private_key_bytes).map_err(|_| MaterialFailure::PrivateKey)?;
    let minimum_bits = match private_key.id() {
        Id::RSA | Id::RSA_PSS => 2_048,
        Id::EC => 256,
        _ => return Err(MaterialFailure::PrivateKey),
    };
    if private_key.bits() < minimum_bits {
        return Err(MaterialFailure::PrivateKey);
    }
    let public_key = leaf.public_key().map_err(|_| MaterialFailure::PrivateKey)?;
    if !public_key.public_eq(&private_key) {
        return Err(MaterialFailure::PrivateKey);
    }
    Ok(dns_names)
}

fn read_certificate(path: &Path) -> Result<(Vec<String>, X509), MaterialFailure> {
    let bytes = read_stable_certificate(path)?;
    let blocks = Pem::iter_from_buffer(&bytes)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| MaterialFailure::Certificate)?;
    if blocks.is_empty() {
        return Err(MaterialFailure::Certificate);
    }
    if blocks.len() > MAX_CERTIFICATES_IN_CHAIN {
        return Err(MaterialFailure::Certificate);
    }
    if blocks.iter().any(|block| block.label != "CERTIFICATE") {
        return Err(MaterialFailure::Certificate);
    }
    for block in &blocks {
        let (remainder, _) =
            parse_x509_certificate(&block.contents).map_err(|_| MaterialFailure::Certificate)?;
        if !remainder.is_empty() {
            return Err(MaterialFailure::Certificate);
        }
    }

    let (_, leaf) =
        parse_x509_certificate(&blocks[0].contents).map_err(|_| MaterialFailure::Certificate)?;
    let subject_alt_name = leaf
        .subject_alternative_name()
        .map_err(|_| MaterialFailure::Certificate)?
        .ok_or(MaterialFailure::Certificate)?;
    let mut dns_names = Vec::new();
    for general_name in &subject_alt_name.value.general_names {
        let GeneralName::DNSName(dns_name) = general_name else {
            continue;
        };
        let dns_name =
            canonical_certificate_dns_name(dns_name).ok_or(MaterialFailure::Certificate)?;
        if dns_names.contains(&dns_name) {
            return Err(MaterialFailure::Certificate);
        }
        dns_names.push(dns_name);
    }
    if dns_names.is_empty() {
        return Err(MaterialFailure::Certificate);
    }
    if dns_names.len() > MAX_CERTIFICATE_DNS_NAMES {
        return Err(MaterialFailure::Certificate);
    }
    let leaf = X509::from_der(&blocks[0].contents).map_err(|_| MaterialFailure::Certificate)?;
    Ok((dns_names, leaf))
}

fn read_stable_certificate(path: &Path) -> Result<Vec<u8>, MaterialFailure> {
    let mut file = File::open(path).map_err(|_| MaterialFailure::Certificate)?;
    let before = file.metadata().map_err(|_| MaterialFailure::Certificate)?;
    if !before.is_file() {
        return Err(MaterialFailure::Certificate);
    }
    if before.len() > u64::try_from(MAX_CERTIFICATE_CHAIN_BYTES).unwrap_or(u64::MAX) {
        return Err(MaterialFailure::Certificate);
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(
            u64::try_from(MAX_CERTIFICATE_CHAIN_BYTES)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        )
        .read_to_end(&mut bytes)
        .map_err(|_| MaterialFailure::Certificate)?;
    let after = file.metadata().map_err(|_| MaterialFailure::Certificate)?;
    if certificate_fingerprint(&before) != certificate_fingerprint(&after) {
        return Err(MaterialFailure::Certificate);
    }
    if bytes.len() > MAX_CERTIFICATE_CHAIN_BYTES {
        return Err(MaterialFailure::Certificate);
    }
    Ok(bytes)
}

fn read_private_key(path: &Path) -> Result<Zeroizing<Vec<u8>>, MaterialFailure> {
    let descriptor = rustix_fs::open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(|_| MaterialFailure::PrivateKey)?;
    let mut file = File::from(descriptor);
    let before = file.metadata().map_err(|_| MaterialFailure::PrivateKey)?;
    if !before.is_file()
        || before.len() > u64::try_from(MAX_PRIVATE_KEY_BYTES).unwrap_or(u64::MAX)
        || !matches!(before.mode() & 0o7777, 0o400 | 0o600 | 0o440 | 0o640)
    {
        return Err(MaterialFailure::PrivateKey);
    }
    let mut bytes = Zeroizing::new(Vec::new());
    (&mut file)
        .take(
            u64::try_from(MAX_PRIVATE_KEY_BYTES)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        )
        .read_to_end(&mut bytes)
        .map_err(|_| MaterialFailure::PrivateKey)?;
    let after = file.metadata().map_err(|_| MaterialFailure::PrivateKey)?;
    if certificate_fingerprint(&before) != certificate_fingerprint(&after)
        || bytes.is_empty()
        || bytes.len() > MAX_PRIVATE_KEY_BYTES
    {
        return Err(MaterialFailure::PrivateKey);
    }
    Ok(bytes)
}

fn has_one_supported_private_key_pem(bytes: &[u8]) -> bool {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let mut begin = None;
    let mut end = None;
    for line in text.lines() {
        if let Some(label) = line
            .strip_prefix("-----BEGIN ")
            .and_then(|line| line.strip_suffix("-----"))
        {
            if begin.replace(label).is_some() {
                return false;
            }
        }
        if let Some(label) = line
            .strip_prefix("-----END ")
            .and_then(|line| line.strip_suffix("-----"))
        {
            if end.replace(label).is_some() {
                return false;
            }
        }
    }
    matches!(
        begin,
        Some("PRIVATE KEY" | "RSA PRIVATE KEY" | "EC PRIVATE KEY")
    ) && end == begin
}

fn certificate_fingerprint(metadata: &std::fs::Metadata) -> CertificateFingerprint {
    CertificateFingerprint {
        length: metadata.len(),
        device: metadata.dev(),
        inode: metadata.ino(),
        modified_seconds: metadata.mtime(),
        modified_nanoseconds: metadata.mtime_nsec(),
        changed_seconds: metadata.ctime(),
        changed_nanoseconds: metadata.ctime_nsec(),
    }
}

fn canonical_certificate_dns_name(name: &str) -> Option<String> {
    let name = name.to_ascii_lowercase();
    if canonical_wildcard_host(&name)
        || (canonical_dns_name(&name) && name.parse::<IpAddr>().is_err())
    {
        Some(name)
    } else {
        None
    }
}

impl Lowerer {
    #[expect(
        clippy::too_many_lines,
        reason = "listener-wide TLS consistency and material lowering are one atomic decision"
    )]
    pub(super) fn lower_tls(
        &mut self,
        http: &EffectiveHttp,
        bind: &EffectiveBind,
        servers: &[&EffectiveServer],
        profile_name: String,
    ) -> Result<Option<LoweredTls>, Vec<LowerIssue>> {
        let mut issues = Vec::new();
        let policies = self.tls_listen_policies(bind, servers, &mut issues);
        let any_tls = policies.iter().any(|policy| policy.tls);
        if !any_tls {
            for policy in policies {
                if policy.h2 {
                    issues.push(issue(
                        &policy.server.origin,
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "plaintext nginx HTTP/2 cannot be represented",
                    ));
                }
            }
            return if issues.is_empty() {
                Ok(None)
            } else {
                Err(issues)
            };
        }
        if matches!(bind.endpoint, ListenEndpoint::Unix { .. }) {
            issues.push(issue(
                &policies[0].server.origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "nginx TLS termination on a Unix listener has no canonical representation",
            ));
        }
        if policies.iter().any(|policy| !policy.tls) {
            issues.push(issue(
                &policies[0].server.origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "virtual servers on one bind disagree about TLS",
            ));
        }

        let mut certificates = Vec::new();
        let mut protocol_policy = None;
        let mut h2_policy = None;
        let mut default_certificate = None;
        for policy in policies {
            let server = policy.server;
            let protocols = self.tls_versions(server.origin.occurrence, &server.origin);
            match protocols {
                Ok((minimum, _)) => {
                    if protocol_policy.is_some_and(|current| current != minimum) {
                        issues.push(issue(
                            &server.origin,
                            E_SEMANTICS_NOT_REPRESENTABLE,
                            "virtual servers on one bind have mismatched TLS protocol policies",
                        ));
                    } else {
                        protocol_policy = Some(minimum);
                    }
                }
                Err(protocol_issues) => issues.extend(protocol_issues),
            }
            if h2_policy.is_some_and(|current| current != policy.h2) {
                issues.push(issue(
                    &server.origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "virtual servers on one bind disagree about HTTP/2",
                ));
            } else {
                h2_policy = Some(policy.h2);
            }
            match self.certificate_for_server(http, server) {
                Ok(certificate) => {
                    if server.origin.occurrence == bind.default_server {
                        default_certificate = Some(certificate.name.clone());
                    }
                    certificates.push(certificate);
                }
                Err(certificate_issues) => issues.extend(certificate_issues),
            }
        }
        if !issues.is_empty() {
            return Err(issues);
        }
        let default_certificate = default_certificate.expect("default server certificate");
        issues.extend(Self::certificate_selection_issues(
            bind,
            servers,
            &certificates,
            &default_certificate,
        ));
        if !issues.is_empty() {
            return Err(issues);
        }
        let mut canonical_certificates = Vec::new();
        let mut certificate_names = Vec::new();
        let mut origins = Vec::new();
        for certificate in certificates {
            origins.push(certificate.origin.clone());
            if certificate_names.contains(&certificate.name) {
                continue;
            }
            certificate_names.push(certificate.name.clone());
            canonical_certificates.push(Certificate {
                name: certificate.name,
                dns_names: certificate.dns_names,
                source: CertificateSource::Files {
                    certificate_chain_path: certificate.chain,
                    private_key_path: certificate.key,
                },
            });
        }
        Ok(Some(LoweredTls {
            certificates: canonical_certificates,
            profile: TlsProfile {
                name: profile_name,
                certificates: certificate_names,
                default_certificate,
                min_version: protocol_policy.expect("validated TLS protocol policy"),
                alpn: if h2_policy == Some(true) {
                    vec![AlpnProtocol::H2, AlpnProtocol::Http11]
                } else {
                    vec![AlpnProtocol::Http11]
                },
            },
            origins,
        }))
    }

    fn certificate_selection_issues(
        bind: &EffectiveBind,
        servers: &[&EffectiveServer],
        certificates: &[CertificateCandidate],
        default_certificate: &str,
    ) -> Vec<LowerIssue> {
        let mut issues = Vec::new();
        let mut names_by_certificate: HashMap<&str, HashSet<String>> = HashMap::new();
        for (server, certificate) in servers.iter().zip(certificates) {
            if server.origin.occurrence == bind.default_server
                || certificate.name == default_certificate
            {
                continue;
            }
            let names = names_by_certificate
                .entry(certificate.name.as_str())
                .or_default();
            for name in &server.server_names {
                if name.kind != ServerNameKind::Exact {
                    continue;
                }
                let Some(host) = utf8(&name.normalized).and_then(canonical_exact_host) else {
                    continue;
                };
                if !certificate.dns_names.contains(&host) {
                    issues.push(issue(
                        &name.origin,
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "certificate SAN ownership cannot preserve nginx virtual-server selection",
                    ));
                }
                names.insert(host);
            }
        }

        let mut checked = HashSet::new();
        for certificate in certificates {
            let name = certificate.name.as_str();
            let Some(server_names) = names_by_certificate.get(name) else {
                continue;
            };
            if !checked.insert(name) {
                continue;
            }
            for dns_name in &certificate.dns_names {
                if dns_name.starts_with("*.") || !server_names.contains(dns_name) {
                    issues.push(issue(
                        &certificate.origin,
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        "non-default certificate SAN ownership exceeds its nginx server_name selection",
                    ));
                }
            }
        }
        issues
    }

    fn tls_listen_policies<'a>(
        &self,
        bind: &EffectiveBind,
        servers: &[&'a EffectiveServer],
        issues: &mut Vec<LowerIssue>,
    ) -> Vec<TlsListenPolicy<'a>> {
        servers
            .iter()
            .map(|server| {
                let listen = matching_listen(server, &bind.endpoint);
                let tls = listen.options.iter().any(|option| option.value == b"ssl");
                let legacy_h2 = listen.options.iter().any(|option| option.value == b"http2");
                let modern_h2 =
                    self.http2_enabled(server.origin.occurrence, &server.origin, issues);
                TlsListenPolicy {
                    server,
                    tls,
                    h2: legacy_h2 || modern_h2,
                }
            })
            .collect()
    }

    fn http2_enabled(
        &self,
        scope: OccurrenceId,
        fallback_origin: &DirectiveOrigin,
        issues: &mut Vec<LowerIssue>,
    ) -> bool {
        let Some(policy) = self.effective_policy(scope, b"http2") else {
            return false;
        };
        match policy.arguments.as_slice() {
            [value] if value == b"on" => true,
            [value] if value == b"off" => false,
            _ => {
                issues.push(issue(
                    policy.origins.last().unwrap_or(fallback_origin),
                    E_INVALID_VALUE,
                    "http2 must be `on` or `off`",
                ));
                false
            }
        }
    }

    fn tls_versions(
        &self,
        scope: OccurrenceId,
        fallback_origin: &DirectiveOrigin,
    ) -> Result<(TlsVersion, Vec<DirectiveOrigin>), Vec<LowerIssue>> {
        let Some(policy) = self.effective_policy(scope, b"ssl_protocols") else {
            return Err(vec![issue(
                fallback_origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "TLS listeners require an explicit TLSv1.2/TLSv1.3 policy",
            )]);
        };
        let versions = policy
            .arguments
            .iter()
            .map(Vec::as_slice)
            .collect::<HashSet<_>>();
        let minimum = if versions == HashSet::from([b"TLSv1.3".as_slice()]) {
            Some(TlsVersion::Tls13)
        } else if versions == HashSet::from([b"TLSv1.2".as_slice(), b"TLSv1.3".as_slice()]) {
            Some(TlsVersion::Tls12)
        } else {
            None
        };
        if let Some(minimum) = minimum {
            Ok((minimum, policy.origins))
        } else {
            Err(vec![issue(
                policy.origins.last().unwrap_or(fallback_origin),
                E_SEMANTICS_NOT_REPRESENTABLE,
                "ssl_protocols is not exactly TLSv1.3 or TLSv1.2 plus TLSv1.3",
            )])
        }
    }

    fn certificate_for_server(
        &mut self,
        http: &EffectiveHttp,
        server: &EffectiveServer,
    ) -> Result<CertificateCandidate, Vec<LowerIssue>> {
        let chains = self.effective_list_policy(
            server.origin.occurrence,
            http.origin.occurrence,
            b"ssl_certificate",
        );
        let keys = self.effective_list_policy(
            server.origin.occurrence,
            http.origin.occurrence,
            b"ssl_certificate_key",
        );
        let mut issues = Vec::new();
        if chains.len() != 1 || keys.len() != 1 {
            issues.push(issue(
                &server.origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "TLS virtual server requires exactly one direct certificate/key pair",
            ));
            return Err(issues);
        }
        let chain = canonical_file_path(&chains[0].arguments[0]);
        let key = canonical_file_path(&keys[0].arguments[0]);
        if chain.is_none() {
            issues.push(issue(
                chains[0].origins.last().unwrap_or(&server.origin),
                E_INVALID_VALUE,
                "ssl_certificate path is not a canonical absolute UTF-8 file path",
            ));
        }
        if key.is_none() {
            issues.push(issue(
                keys[0].origins.last().unwrap_or(&server.origin),
                E_INVALID_VALUE,
                "ssl_certificate_key path is not a canonical absolute UTF-8 file path",
            ));
        }
        if chain == key {
            issues.push(issue(
                &server.origin,
                E_INVALID_VALUE,
                "certificate and private-key paths must differ",
            ));
        }
        if !issues.is_empty() {
            return Err(issues);
        }
        let chain = chain.expect("checked chain path");
        let key = key.expect("checked key path");
        let identity = CertificateIdentity {
            chain: chain.clone(),
            key: key.clone(),
        };
        let metadata = if let Some(metadata) = self.certificate_identities.get(&identity) {
            metadata.clone()
        } else {
            let dns_names = load_certificate_identity(&chain, &key).map_err(|failure| {
                let (origin, message) = match failure {
                    MaterialFailure::Certificate => (
                        chains[0].origins.last().unwrap_or(&server.origin),
                        "certificate metadata is unreadable or unsupported",
                    ),
                    MaterialFailure::PrivateKey => (
                        keys[0].origins.last().unwrap_or(&server.origin),
                        "private key material is unreadable or unsupported",
                    ),
                };
                vec![issue(origin, E_INVALID_VALUE, message)]
            })?;
            let metadata = CertificateMetadata {
                name: format!("nginx-certificate-{}", self.certificate_identities.len()),
                dns_names,
            };
            self.certificate_identities
                .insert(identity.clone(), metadata.clone());
            metadata
        };
        Ok(CertificateCandidate {
            name: metadata.name,
            dns_names: metadata.dns_names,
            chain,
            key,
            origin: server.origin.clone(),
        })
    }
}
