use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

use oxiroute_config::{AlpnProtocol, Certificate, CertificateSource, TlsProfile, TlsVersion};

use crate::{E_INVALID_VALUE, E_SEMANTICS_NOT_REPRESENTABLE};

use crate::nginx::{
    DirectiveOrigin, EffectiveBind, EffectiveHttp, EffectiveServer, ListenEndpoint, OccurrenceId,
    ServerNameKind,
};

use super::{
    LowerIssue, Lowerer,
    listener::{canonical_exact_host, canonical_wildcard_host, matching_listen},
    provenance::{issue, utf8},
};

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
}

struct TlsListenPolicy<'a> {
    server: &'a EffectiveServer,
    tls: bool,
    h2: bool,
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

fn certificate_source(chain: PathBuf, key: PathBuf) -> CertificateSource {
    let live_directory = chain.parent();
    let certbot_root = live_directory.and_then(Path::parent);
    let certbot_domain = live_directory.and_then(Path::file_name);
    if chain
        .file_name()
        .is_some_and(|name| name == "fullchain.pem")
        && key.file_name().is_some_and(|name| name == "privkey.pem")
        && key.parent() == live_directory
        && certbot_root
            .and_then(Path::file_name)
            .is_some_and(|name| name == "live")
    {
        let root = certbot_root
            .and_then(Path::parent)
            .expect("live directory has a parent");
        return CertificateSource::Certbot {
            live_directory_path: live_directory
                .expect("certbot live directory")
                .to_path_buf(),
            archive_directory_path: root
                .join("archive")
                .join(certbot_domain.expect("certbot domain directory")),
        };
    }
    CertificateSource::Files {
        certificate_chain_path: chain,
        private_key_path: key,
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
            for directive in [
                b"ssl_ciphers".as_slice(),
                b"ssl_dhparam",
                b"ssl_session_cache",
                b"ssl_session_tickets",
                b"ssl_session_timeout",
            ] {
                if let Some(value) = self.effective_policy(server.origin.occurrence, directive) {
                    issues.push(issue(
                        value.origins.last().unwrap_or(&server.origin),
                        E_SEMANTICS_NOT_REPRESENTABLE,
                        format!(
                            "explicit {} policy is not represented by canonical TLS profiles",
                            String::from_utf8_lossy(directive)
                        ),
                    ));
                }
            }
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
                    "virtual servers on one bind have mismatched HTTP/2 policies",
                ));
            } else {
                h2_policy = Some(policy.h2);
            }
            match self.certificate_for_server(http, bind, server) {
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
        let mut canonical_certificates = Vec::new();
        let mut certificate_names = Vec::new();
        let mut origins = Vec::new();
        for certificate in certificates {
            origins.push(certificate.origin.clone());
            if let Some(existing) = canonical_certificates
                .iter_mut()
                .find(|existing: &&mut Certificate| existing.name == certificate.name)
            {
                for dns_name in certificate.dns_names {
                    if !existing.dns_names.contains(&dns_name) {
                        existing.dns_names.push(dns_name);
                    }
                }
                continue;
            }
            certificate_names.push(certificate.name.clone());
            canonical_certificates.push(Certificate {
                name: certificate.name,
                dns_names: certificate.dns_names,
                source: certificate_source(certificate.chain, certificate.key),
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

    #[expect(
        clippy::too_many_lines,
        reason = "certificate inheritance, identity, and source validation are one atomic decision"
    )]
    fn certificate_for_server(
        &mut self,
        http: &EffectiveHttp,
        bind: &EffectiveBind,
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
        self.used_certificate_overlays.borrow_mut().extend(
            chains
                .iter()
                .chain(&keys)
                .flat_map(|policy| policy.origins.iter().map(|origin| origin.occurrence)),
        );
        let identity = CertificateIdentity {
            chain: chain.clone(),
            key: key.clone(),
        };
        let metadata = if let Some(metadata) = self.certificate_identities.get(&identity) {
            metadata.clone()
        } else {
            let metadata = CertificateMetadata {
                name: format!("nginx-certificate-{}", self.certificate_identities.len()),
            };
            self.certificate_identities
                .insert(identity.clone(), metadata.clone());
            metadata
        };
        let mut dns_names = Vec::new();
        for name in bind
            .names
            .iter()
            .filter(|name| name.server == server.origin.occurrence)
            .map(|name| &name.name)
        {
            match name.kind {
                ServerNameKind::Exact => {
                    if let Some(name) = utf8(&name.normalized).and_then(canonical_exact_host) {
                        dns_names.push(name);
                    }
                }
                ServerNameKind::LeadingWildcard => {
                    if let Some(name) = utf8(&name.normalized).and_then(|name| {
                        canonical_wildcard_host(name).then(|| name.to_ascii_lowercase())
                    }) {
                        dns_names.push(name);
                    }
                }
                ServerNameKind::LeadingWildcardAndExact => {
                    if let Some(suffix) = utf8(&name.normalized)
                        .and_then(|name| name.strip_prefix('.'))
                        .filter(|suffix| super::listener::canonical_dns_name(suffix))
                    {
                        dns_names.push(suffix.to_ascii_lowercase());
                        dns_names.push(format!("*.{}", suffix.to_ascii_lowercase()));
                    }
                }
                _ => {}
            }
        }
        dns_names.sort();
        dns_names.dedup();
        if dns_names.is_empty() {
            return Err(vec![issue(
                &server.origin,
                E_SEMANTICS_NOT_REPRESENTABLE,
                "TLS virtual server needs a canonical DNS or IP server_name for certificate activation",
            )]);
        }
        Ok(CertificateCandidate {
            name: metadata.name,
            dns_names,
            chain,
            key,
            origin: server.origin.clone(),
        })
    }
}
