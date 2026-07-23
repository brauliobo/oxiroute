use std::{net::IpAddr, time::Duration};

use crate::Span;

use super::super::{OccurrenceId, Provenance, Word};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeValue {
    pub value: Vec<u8>,
    pub span: Span,
}

impl From<&Word> for NativeValue {
    fn from(word: &Word) -> Self {
        Self {
            value: word.value.clone(),
            span: word.span,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectiveOrigin {
    pub occurrence: OccurrenceId,
    pub directive_span: Span,
    pub name_span: Span,
    pub argument_spans: Vec<Span>,
    pub provenance: Provenance,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DirectiveFamily {
    Include,
    Acl,
    Access,
    Port,
    CachePeer,
    Refresh,
    CachePolicy,
    Storage,
    Authentication,
    Logging,
    Dns,
    Privacy,
    Process,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum DirectiveSemantics {
    Include,
    AclSource,
    AclPort,
    AclProxyAuth,
    AclUnsupported,
    HttpAccess,
    HeaderAccess,
    DirectAccess,
    CacheAccess,
    HttpPort,
    HttpsPort,
    IcpPort,
    HtcpPort,
    CachePeer,
    RefreshPattern,
    CacheSetting,
    StorageSetting,
    AuthenticationHelper,
    AuthenticationRealm,
    AuthenticationCredentialTtl,
    AuthenticationSetting,
    AccessLogging,
    LoggingSetting,
    DnsNameservers,
    DnsSetting,
    ForwardedFor,
    Via,
    HeaderPrivacy,
    CoreDumpDirectory,
    ProcessSetting,
    Unknown,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum SemanticBlockerKind {
    IncludeExpansion,
    ForwardProxyListener,
    SourceAddressAcl,
    DestinationPortAcl,
    ProxyAuthenticationAcl,
    OrderedHttpAccess,
    HeaderAccessPolicy,
    DirectRoutingPolicy,
    CacheAccessPolicy,
    CachePeerHierarchy,
    RefreshPolicy,
    CachePolicy,
    StoragePolicy,
    ProxyAuthentication,
    AccessLoggingPolicy,
    LoggingPolicy,
    ResolverPolicy,
    ForwardedForPolicy,
    ViaPolicy,
    HeaderPrivacyPolicy,
    UnsupportedPortOption,
    UnsupportedAclType,
    ConflictingAclType,
    UnresolvedAclReference,
    InvalidForm,
    UnknownDirective,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Activation {
    Structural,
    Externalized,
    Blocked(SemanticBlockerKind),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DirectiveResolution {
    Structural,
    Append,
    MergeSameName,
    OrderedFirstMatch,
    LastWins,
    Externalized,
    Blocked,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecisionOutcome {
    Classified {
        family: DirectiveFamily,
        semantics: DirectiveSemantics,
        resolution: DirectiveResolution,
        activation: Activation,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Decision {
    pub origin: DirectiveOrigin,
    pub name: Vec<u8>,
    pub outcome: DecisionOutcome,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DecisionLedger {
    pub decisions: Vec<Decision>,
}

impl DecisionLedger {
    #[must_use]
    pub fn decision(&self, occurrence: OccurrenceId) -> Option<&Decision> {
        self.decisions
            .get(occurrence.get())
            .filter(|decision| decision.origin.occurrence == occurrence)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IpNetwork {
    pub address: IpAddr,
    pub prefix_length: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PortRange {
    pub start: u16,
    pub end: u16,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SecretKind {
    AuthenticationHelper,
    AuthenticationRealm,
    ProxyIdentity,
    PeerCredentials,
    BearerToken,
    PasswordHash,
    PrivateKey,
    UnknownCredential,
}

/// Typed evidence that a source range is secret-bearing. It deliberately has no value field.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SecretFact {
    pub kind: SecretKind,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProxyAuthMatcher {
    Required,
    Identity(SecretFact),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AclMatcher {
    Source(IpNetwork),
    Port(PortRange),
    ProxyAuth(ProxyAuthMatcher),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AclType {
    Source,
    Port,
    ProxyAuth,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AclDefinition {
    pub origin: DirectiveOrigin,
    pub name: NativeValue,
    pub acl_type: AclType,
    pub matchers: Vec<AclMatcher>,
}

/// Same-name declarations of the same type are merged with OR semantics in declaration order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveAcl {
    pub name: Vec<u8>,
    pub acl_type: AclType,
    pub definitions: Vec<OccurrenceId>,
    pub matchers: Vec<AclMatcher>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessAction {
    Allow,
    Deny,
}

impl AccessAction {
    #[must_use]
    pub const fn opposite(self) -> Self {
        match self {
            Self::Allow => Self::Deny,
            Self::Deny => Self::Allow,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BuiltinAcl {
    All,
    Connect,
    Localhost,
    Manager,
    ToLocalhost,
    ToLinkLocal,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AclReferenceResolution {
    Defined(Vec<OccurrenceId>),
    Builtin(BuiltinAcl),
    Unresolved,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AclTerm {
    pub negated: bool,
    pub name: NativeValue,
    pub resolution: AclReferenceResolution,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AccessListKind {
    Http,
    RequestHeader,
    ReplyHeader,
    FollowForwardedFor,
    AlwaysDirect,
    NeverDirect,
    Cache,
    CachePeer,
    Other,
}

/// One Squid rule. Terms are AND-combined; repeated lines use first-match source order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccessRule {
    pub origin: DirectiveOrigin,
    pub kind: AccessListKind,
    pub selector: Option<NativeValue>,
    pub action: AccessAction,
    pub terms: Vec<AclTerm>,
    pub order: usize,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccessPolicy {
    pub kind: AccessListKind,
    pub selector: Option<Vec<u8>>,
    pub rules: Vec<AccessRule>,
    /// Squid uses the opposite of the last rule when no rule matches. Empty HTTP policy denies.
    pub default_action: AccessAction,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessEvaluation {
    Decided {
        action: AccessAction,
        matched_rule: Option<OccurrenceId>,
    },
    Indeterminate {
        rule: OccurrenceId,
    },
}

impl AccessPolicy {
    /// Evaluates ordered first-match semantics using caller-supplied ACL truth values.
    #[must_use]
    pub fn evaluate<F>(&self, mut evaluate_term: F) -> AccessEvaluation
    where
        F: FnMut(&AclTerm) -> Option<bool>,
    {
        for rule in &self.rules {
            let mut unknown = false;
            let mut matched = true;
            for term in &rule.terms {
                match evaluate_term(term).map(|value| value ^ term.negated) {
                    Some(true) => {}
                    Some(false) => {
                        matched = false;
                        break;
                    }
                    None => unknown = true,
                }
            }
            if matched && unknown {
                return AccessEvaluation::Indeterminate {
                    rule: rule.origin.occurrence,
                };
            }
            if matched {
                return AccessEvaluation::Decided {
                    action: rule.action,
                    matched_rule: Some(rule.origin.occurrence),
                };
            }
        }
        AccessEvaluation::Decided {
            action: self.default_action,
            matched_rule: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PortKind {
    Http,
    Https,
    Icp,
    Htcp,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PortEndpoint {
    Wildcard { port: u16 },
    Ip { address: IpAddr, port: u16 },
    Host { host: Vec<u8>, port: u16 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PortOption {
    Intercept,
    Tproxy,
    Accel,
    SslBump,
    Name(NativeValue),
    DefaultSite(NativeValue),
    Unsupported(NativeValue),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortDirective {
    pub origin: DirectiveOrigin,
    pub kind: PortKind,
    pub endpoint: PortEndpoint,
    pub options: Vec<PortOption>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CachePeerType {
    Parent,
    Sibling,
    Multicast,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PeerOption {
    NoQuery,
    ProxyOnly,
    OriginServer,
    RoundRobin,
    Weight(u32),
    Name(NativeValue),
    Secret(SecretFact),
    Unsupported(NativeValue),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CachePeer {
    pub origin: DirectiveOrigin,
    pub host: NativeValue,
    pub peer_type: CachePeerType,
    pub http_port: u16,
    pub icp_port: u16,
    pub options: Vec<PeerOption>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefreshOption {
    OverrideExpire,
    OverrideLastModified,
    ReloadIntoIms,
    IgnoreReload,
    IgnoreNoStore,
    IgnorePrivate,
    RefreshIms,
    StoreStale,
    MaxStale(u64),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RefreshPattern {
    pub origin: DirectiveOrigin,
    pub case_insensitive: bool,
    pub pattern: NativeValue,
    pub minimum: Duration,
    pub percent: u8,
    pub maximum: Duration,
    pub options: Vec<RefreshOption>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RefreshPolicy {
    /// Squid selects the first matching pattern.
    pub patterns: Vec<RefreshPattern>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthenticationSetting {
    Program,
    Children,
    Concurrency,
    Realm,
    CredentialTtl,
    CaseSensitive,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AuthenticationValue {
    Helper(SecretFact),
    Realm(SecretFact),
    Duration(Duration),
    Count(u32),
    Boolean(bool),
    Opaque { argument_count: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthenticationParameter {
    pub origin: DirectiveOrigin,
    pub scheme: NativeValue,
    pub setting: AuthenticationSetting,
    pub value: AuthenticationValue,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthenticationScheme {
    pub scheme: Vec<u8>,
    /// Last declaration wins for each scalar setting; all origins remain in `parameters`.
    pub parameters: Vec<OccurrenceId>,
    pub program: Option<SecretFact>,
    pub realm: Option<SecretFact>,
    pub credential_ttl: Option<Duration>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LogDestination {
    Disabled,
    Stdio(NativeValue),
    Daemon(NativeValue),
    Syslog(Option<NativeValue>),
    File(NativeValue),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoggingDirective {
    pub origin: DirectiveOrigin,
    pub destination: LogDestination,
    pub format: Option<NativeValue>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DnsNameservers {
    pub origin: DirectiveOrigin,
    pub addresses: Vec<IpAddr>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForwardedForMode {
    On,
    Off,
    Transparent,
    Delete,
    Truncate,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PrivacyDirective {
    ForwardedFor {
        origin: DirectiveOrigin,
        mode: ForwardedForMode,
    },
    Via {
        origin: DirectiveOrigin,
        enabled: bool,
    },
    HeaderReplace {
        origin: DirectiveOrigin,
        request: bool,
        name: NativeValue,
        replacement: Vec<NativeValue>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CacheDirective {
    MemoryBytes {
        origin: DirectiveOrigin,
        bytes: u64,
    },
    Toggle {
        origin: DirectiveOrigin,
        name: Vec<u8>,
        enabled: bool,
    },
    Scalar {
        origin: DirectiveOrigin,
        name: Vec<u8>,
        values: Vec<NativeValue>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StorageDirective {
    CacheDir {
        origin: DirectiveOrigin,
        storage_type: NativeValue,
        path: NativeValue,
        size_mib: u64,
        level_one: u32,
        level_two: u32,
        options: Vec<NativeValue>,
    },
    Opaque {
        origin: DirectiveOrigin,
        name: Vec<u8>,
        argument_count: usize,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProcessDirective {
    CoreDumpDirectory {
        origin: DirectiveOrigin,
        path: NativeValue,
    },
    Opaque {
        origin: DirectiveOrigin,
        name: Vec<u8>,
        argument_count: usize,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpaqueDirective {
    pub origin: DirectiveOrigin,
    pub name: Vec<u8>,
    pub argument_count: usize,
    pub secret: Option<SecretFact>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EffectiveConfiguration {
    pub acl_definitions: Vec<AclDefinition>,
    pub acls: Vec<EffectiveAcl>,
    pub access_rules: Vec<AccessRule>,
    pub access_policies: Vec<AccessPolicy>,
    pub ports: Vec<PortDirective>,
    pub cache_peers: Vec<CachePeer>,
    pub refresh_policy: RefreshPolicy,
    pub cache_policy: Vec<CacheDirective>,
    pub storage: Vec<StorageDirective>,
    pub authentication: Vec<AuthenticationParameter>,
    pub authentication_schemes: Vec<AuthenticationScheme>,
    pub authentication_controls: Vec<OpaqueDirective>,
    pub logging: Vec<LoggingDirective>,
    pub dns_nameservers: Vec<DnsNameservers>,
    pub dns_controls: Vec<OpaqueDirective>,
    pub privacy: Vec<PrivacyDirective>,
    pub process: Vec<ProcessDirective>,
    pub ledger: DecisionLedger,
}
