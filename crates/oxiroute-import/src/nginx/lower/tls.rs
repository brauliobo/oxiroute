use std::{
    collections::{HashMap, HashSet},
    path::PathBuf,
};

use oxiroute_config::{
    AlpnProtocol, Certificate, CertificateSource, TlsPolicy, TlsProfile, TlsSessionCache,
    TlsVersion,
};

use crate::{
    Diagnostic, DiagnosticCode, E_DUPLICATE_IDENTITY, E_INVALID_VALUE,
    E_SEMANTICS_NOT_REPRESENTABLE, E_SOURCE_IO, canonical::absolute_file_path,
};

use crate::nginx::{
    DirectiveOrigin, EffectiveBind, EffectiveHttp, EffectiveServer, ListenEndpoint, OccurrenceId,
};

use super::{
    LowerIssue, Lowerer,
    listener::{bind_listens, parse_duration_ms, parse_size},
    provenance::{issue, utf8, warning},
};

mod identity;

use identity::{certificate_source, declared_identities, lineage_identities, uncovered_identities};

/// A listen-flag `http2` on one server enables HTTP/2 for every server on the socket.
const W_HTTP2_SOCKET_WIDE: DiagnosticCode = DiagnosticCode::new("W_HTTP2_SOCKET_WIDE");
/// A `server_name` is not among the subject alternative names of the certificate its server uses.
const W_CERTIFICATE_NAME_UNCOVERED: DiagnosticCode =
    DiagnosticCode::new("W_CERTIFICATE_NAME_UNCOVERED");

struct CertificateCandidate {
    name: String,
    dns_names: Vec<String>,
    uncovered: Vec<String>,
    source: CertificateSource,
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
    /// Leaf SANs of a Certbot lineage, read once per lineage; `None` for sources named by `server_name`.
    lineage: Option<Result<Vec<String>, String>>,
}

struct TlsListenPolicy<'a> {
    server: &'a EffectiveServer,
    tls: bool,
    listen_h2: bool,
    directive_h2: bool,
}

impl TlsListenPolicy<'_> {
    const fn h2(&self) -> bool {
        self.listen_h2 || self.directive_h2
    }
}

/// nginx negotiates ALPN per socket before it selects a server, so a `listen ... http2` flag on any server
/// enables HTTP/2 for all of them. Without a flag only the `http2` directive applies, per selected server, which
/// one canonical TLS profile can honor only when every server agrees.
fn bind_h2(
    policies: &[TlsListenPolicy<'_>],
    warnings: &mut Vec<Diagnostic>,
    issues: &mut Vec<LowerIssue>,
) -> bool {
    if policies.iter().any(|policy| policy.listen_h2) {
        warnings.extend(policies.iter().filter(|policy| !policy.h2()).map(|policy| {
            warning(
                &policy.server.origin,
                W_HTTP2_SOCKET_WIDE,
                "nginx negotiates HTTP/2 for every server on this socket because another server enables it in listen",
            )
        }));
        return true;
    }
    let enabled = policies[0].directive_h2;
    issues.extend(
        policies
            .iter()
            .filter(|policy| policy.directive_h2 != enabled)
            .map(|policy| {
                issue(
                    &policy.server.origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "virtual servers on one bind have mismatched HTTP/2 policies and no listen flag; nginx would decide per server name, a TLS profile cannot",
                )
            }),
    );
    enabled
}

fn shared_identity_issues(candidates: &[CertificateCandidate]) -> Vec<LowerIssue> {
    let mut claims = HashMap::new();
    let mut issues = Vec::new();
    for candidate in candidates {
        for name in &candidate.dns_names {
            if claims
                .insert(name, &candidate.name)
                .is_some_and(|first| *first != candidate.name)
            {
                issues.push(issue(
                    &candidate.origin,
                    E_DUPLICATE_IDENTITY,
                    format!(
                        "two certificates in one TLS profile claim the subject alternative name `{name}`"
                    ),
                ));
            }
        }
    }
    issues
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
                if policy.h2() {
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
        let mut tls_policy = None;
        let mut default_certificate = None;
        let mut origins = Vec::new();
        let mut warnings = Vec::new();
        let h2 = bind_h2(&policies, &mut warnings, &mut issues);
        for policy in &policies {
            let server = policy.server;
            match self.tls_policy(server.origin.occurrence, &server.origin) {
                Ok((policy, policy_origins)) => {
                    origins.extend(policy_origins);
                    if tls_policy
                        .as_ref()
                        .is_some_and(|current| current != &policy)
                    {
                        issues.push(issue(
                            &server.origin,
                            E_SEMANTICS_NOT_REPRESENTABLE,
                            "virtual servers on one bind have mismatched TLS policies",
                        ));
                    } else {
                        tls_policy = Some(policy);
                    }
                }
                Err(policy_issues) => issues.extend(policy_issues),
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
            match self.certificate_for_server(http, bind, server) {
                Ok(certificate) => {
                    if server.origin.occurrence == bind.default_server {
                        default_certificate = Some(certificate.name.clone());
                    }
                    warnings.extend(certificate.uncovered.iter().map(|name| {
                        warning(
                            &server.origin,
                            W_CERTIFICATE_NAME_UNCOVERED,
                            format!(
                                "server_name `{name}` is not among the subject alternative names of its certificate; nginx would still serve that certificate, oxiroute serves the profile default for this name"
                            ),
                        )
                    }));
                    certificates.push(certificate);
                }
                Err(certificate_issues) => issues.extend(certificate_issues),
            }
        }
        issues.extend(shared_identity_issues(&certificates));
        if !issues.is_empty() {
            return Err(issues);
        }
        self.diagnostics.extend(warnings);
        let default_certificate = default_certificate.expect("default server certificate");
        let mut canonical_certificates = Vec::new();
        let mut certificate_names = Vec::new();
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
                source: certificate.source,
            });
        }
        Ok(Some(LoweredTls {
            certificates: canonical_certificates,
            profile: TlsProfile {
                name: profile_name,
                certificates: certificate_names,
                default_certificate,
                min_version: protocol_policy.expect("validated TLS protocol policy"),
                alpn: if h2 {
                    vec![AlpnProtocol::H2, AlpnProtocol::Http11]
                } else {
                    vec![AlpnProtocol::Http11]
                },
                policy: tls_policy.expect("validated TLS policy"),
            },
            origins,
        }))
    }

    fn tls_policy(
        &self,
        scope: OccurrenceId,
        fallback_origin: &DirectiveOrigin,
    ) -> Result<(TlsPolicy, Vec<DirectiveOrigin>), Vec<LowerIssue>> {
        let mut policy = TlsPolicy::default();
        let mut origins = Vec::new();
        let mut issues = Vec::new();

        if let Some(value) = self.effective_policy(scope, b"ssl_ciphers") {
            let origin = value.origins.last().unwrap_or(fallback_origin);
            origins.extend(value.origins.clone());
            match value.arguments.as_slice() {
                [cipher_list] if utf8(cipher_list).is_some_and(|value| !value.is_empty()) => {
                    policy.cipher_list = utf8(cipher_list).map(str::to_owned);
                }
                _ => issues.push(issue(
                    origin,
                    E_INVALID_VALUE,
                    "ssl_ciphers must be one nonempty UTF-8 cipher string",
                )),
            }
        }
        if let Some(value) = self.effective_policy(scope, b"ssl_dhparam") {
            let origin = value.origins.last().unwrap_or(fallback_origin);
            origins.extend(value.origins.clone());
            match value.arguments.as_slice() {
                [path] if absolute_file_path(path).is_some() => {
                    policy.dh_parameters_path = absolute_file_path(path);
                }
                _ => issues.push(issue(
                    origin,
                    E_INVALID_VALUE,
                    "ssl_dhparam must be one canonical absolute file path",
                )),
            }
        }
        if let Some(value) = self.effective_policy(scope, b"ssl_session_cache") {
            let origin = value.origins.last().unwrap_or(fallback_origin);
            origins.extend(value.origins.clone());
            match value.arguments.as_slice() {
                [cache] if parse_shared_session_cache(cache).is_some() => {
                    policy.session_cache = parse_shared_session_cache(cache);
                }
                _ => issues.push(issue(
                    origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "ssl_session_cache must use one shared:name:size cache",
                )),
            }
        }
        if let Some(value) = self.effective_policy(scope, b"ssl_session_timeout") {
            let origin = value.origins.last().unwrap_or(fallback_origin);
            origins.extend(value.origins.clone());
            match value.arguments.as_slice() {
                [timeout] if parse_session_timeout_seconds(timeout).is_some() => {
                    policy.session_timeout_seconds = parse_session_timeout_seconds(timeout);
                }
                _ => issues.push(issue(
                    origin,
                    E_INVALID_VALUE,
                    "ssl_session_timeout must be a positive whole-second nginx duration",
                )),
            }
        }
        for (directive, target) in [
            (
                b"ssl_session_tickets".as_slice(),
                &mut policy.session_tickets,
            ),
            (
                b"ssl_prefer_server_ciphers".as_slice(),
                &mut policy.prefer_server_ciphers,
            ),
        ] {
            if let Some(value) = self.effective_policy(scope, directive) {
                let origin = value.origins.last().unwrap_or(fallback_origin);
                origins.extend(value.origins.clone());
                match value.arguments.as_slice() {
                    [value] if value == b"on" => *target = true,
                    [value] if value == b"off" => *target = false,
                    _ => issues.push(issue(
                        origin,
                        E_INVALID_VALUE,
                        format!(
                            "{} must be `on` or `off`",
                            String::from_utf8_lossy(directive)
                        ),
                    )),
                }
            }
        }

        if issues.is_empty() {
            Ok((policy, origins))
        } else {
            Err(issues)
        }
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
                let has_option = |name: &[u8]| {
                    bind_listens(server, bind)
                        .any(|listen| listen.options.iter().any(|option| option.value == name))
                };
                TlsListenPolicy {
                    server,
                    tls: has_option(b"ssl"),
                    listen_h2: has_option(b"http2"),
                    directive_h2: self.http2_enabled(
                        server.origin.occurrence,
                        &server.origin,
                        issues,
                    ),
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
        let chain = absolute_file_path(&chains[0].arguments[0]);
        let key = absolute_file_path(&keys[0].arguments[0]);
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
        let source = certificate_source(chain.clone(), key.clone());
        let next = self.certificate_identities.len();
        let metadata = self
            .certificate_identities
            .entry(CertificateIdentity { chain, key })
            .or_insert_with(|| CertificateMetadata {
                name: format!("nginx-certificate-{next}"),
                lineage: lineage_identities(&source),
            })
            .clone();
        let mut dns_names = declared_identities(bind, server);
        let mut uncovered = Vec::new();
        match metadata.lineage {
            None if dns_names.is_empty() => {
                return Err(vec![issue(
                    &server.origin,
                    E_SEMANTICS_NOT_REPRESENTABLE,
                    "TLS virtual server needs a canonical DNS or IP server_name for certificate activation",
                )]);
            }
            None => {}
            Some(Err(reason)) => {
                return Err(vec![issue(
                    chains[0].origins.last().unwrap_or(&server.origin),
                    E_SOURCE_IO,
                    format!("certificate identity evidence unavailable: {reason}"),
                )]);
            }
            Some(Ok(identities)) => {
                uncovered = uncovered_identities(&dns_names, &identities);
                dns_names = identities;
            }
        }
        Ok(CertificateCandidate {
            name: metadata.name,
            dns_names,
            uncovered,
            source,
            origin: server.origin.clone(),
        })
    }
}

fn parse_shared_session_cache(value: &[u8]) -> Option<TlsSessionCache> {
    let value = value.strip_prefix(b"shared:")?;
    let separator = value.iter().rposition(|byte| *byte == b':')?;
    let name = utf8(&value[..separator])?;
    if name.is_empty()
        || name.len() > 255
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
    {
        return None;
    }
    Some(TlsSessionCache {
        name: name.into(),
        size_bytes: parse_size(&value[separator + 1..])?,
    })
}

fn parse_session_timeout_seconds(value: &[u8]) -> Option<u64> {
    let milliseconds = parse_duration_ms(value)?;
    (milliseconds % 1_000 == 0).then_some(milliseconds / 1_000)
}
