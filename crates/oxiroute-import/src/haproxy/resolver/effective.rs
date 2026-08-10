/// A recognized directive has a form or section placement that this resolver cannot represent.
pub const E_UNSUPPORTED_FORM: DiagnosticCode = DiagnosticCode::new("E_UNSUPPORTED_FORM");
/// A parsed section is outside the first `HAProxy` semantic subset.
pub const E_UNSUPPORTED_SECTION: DiagnosticCode = DiagnosticCode::new("E_UNSUPPORTED_SECTION");
/// A directive name is not registered by this resolver.
pub const E_UNKNOWN_DIRECTIVE: DiagnosticCode = DiagnosticCode::new("E_UNKNOWN_DIRECTIVE");
/// An occurrence reached the terminal accounting pass without a semantic decision.
pub const E_UNCONSUMED_DIRECTIVE: DiagnosticCode = DiagnosticCode::new("E_UNCONSUMED_DIRECTIVE");
/// `HAProxy` statistics behavior is retained but cannot be activated by this import subset.
pub const E_STATS_UNSUPPORTED: DiagnosticCode = DiagnosticCode::new("E_STATS_UNSUPPORTED");
/// `HAProxy` logging behavior is retained but cannot be activated by this import subset.
pub const E_LOGGING_UNSUPPORTED: DiagnosticCode = DiagnosticCode::new("E_LOGGING_UNSUPPORTED");
/// A setting belongs to process or deployment ownership rather than proxy routing semantics.
pub const E_PROCESS_OWNED: DiagnosticCode = DiagnosticCode::new("E_PROCESS_OWNED");
/// Two direct settings in one section request different effective values.
pub const E_CONFLICTING_DIRECTIVE: DiagnosticCode = DiagnosticCode::new("E_CONFLICTING_DIRECTIVE");

const MAX_CERTIFICATE_CHAIN_BYTES: usize = 1024 * 1024;
const MAX_CERTIFICATES_IN_CHAIN: usize = 16;
const MAX_CERTIFICATE_DNS_NAMES: usize = 100;

/// Stable identity of one section occurrence in the ordered parsed sources.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct SectionId {
    pub source: SourceId,
    pub section_ordinal: usize,
}
/// Stable identity of every parsed `HAProxy` statement occurrence.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum OccurrenceId {
    Preamble {
        source: SourceId,
        directive_ordinal: usize,
    },
    SectionHeader(SectionId),
    SectionDirective {
        section: SectionId,
        directive_ordinal: usize,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DefaultsSelection {
    ImplicitLatest,
    Explicit,
}

/// One defaults copy in a value's complete inheritance path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InheritanceStep {
    pub source_defaults: SectionId,
    pub destination: SectionId,
    pub selection: DefaultsSelection,
    pub reference_span: Option<Span>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceTarget {
    pub occurrence: OccurrenceId,
    pub span: Span,
}

/// Exact source and target locations for a resolved semantic reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReferenceProvenance {
    pub use_span: Span,
    pub targets: Vec<ReferenceTarget>,
}

/// Direct source, defaults path, and optional reference target for one effective value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Provenance {
    pub origin: OccurrenceId,
    pub origin_span: Span,
    pub inheritance: Vec<InheritanceStep>,
    pub references: Vec<ReferenceProvenance>,
}

impl Provenance {
    #[must_use]
    pub const fn is_direct(&self) -> bool {
        self.inheritance.is_empty()
    }

    #[must_use]
    pub const fn is_inherited(&self) -> bool {
        !self.inheritance.is_empty()
    }

    #[must_use]
    pub const fn is_reference(&self) -> bool {
        !self.references.is_empty()
    }

    fn direct(origin: OccurrenceId, origin_span: Span) -> Self {
        Self {
            origin,
            origin_span,
            inheritance: Vec::new(),
            references: Vec::new(),
        }
    }

    fn with_reference(mut self, use_span: Span, targets: Vec<ReferenceTarget>) -> Self {
        self.references
            .push(ReferenceProvenance { use_span, targets });
        self
    }

    fn inherit(&mut self, step: InheritanceStep) {
        self.inheritance.push(step);
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveValue<T> {
    pub value: T,
    pub provenance: Provenance,
}

impl<T> EffectiveValue<T> {
    fn direct(value: T, occurrence: OccurrenceId, span: Span) -> Self {
        Self {
            value,
            provenance: Provenance::direct(occurrence, span),
        }
    }

    fn direct_reference(
        value: T,
        occurrence: OccurrenceId,
        span: Span,
        targets: Vec<ReferenceTarget>,
    ) -> Self {
        Self {
            value,
            provenance: Provenance::direct(occurrence, span).with_reference(span, targets),
        }
    }

    fn direct_references(
        value: T,
        occurrence: OccurrenceId,
        span: Span,
        references: Vec<ReferenceProvenance>,
    ) -> Self {
        Self {
            value,
            provenance: Provenance {
                origin: occurrence,
                origin_span: span,
                inheritance: Vec::new(),
                references,
            },
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveSection {
    pub id: SectionId,
    pub declaration: OccurrenceId,
    pub name: Option<Vec<u8>>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DefaultsSource {
    pub section: SectionId,
    pub selection: DefaultsSelection,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProxyMode {
    Http,
    Tcp,
    Unsupported(Vec<u8>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SemanticBlockerKind {
    ConflictingDirective,
    GlobalSecurity,
    Logging,
    Mode,
    ProxyDefault,
    Retry,
    Timeout,
    Tls,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SemanticBlocker {
    pub kind: SemanticBlockerKind,
    pub keyword: Vec<u8>,
    pub arguments: Vec<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BalanceAlgorithm {
    RoundRobin,
    LeastConnections,
    First,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BackendReference {
    pub name: Vec<u8>,
    pub target: SectionId,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Redispatch {
    pub interval: Option<i32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ForwardFor {
    pub except: Option<Vec<u8>>,
    pub header: Option<Vec<u8>>,
    pub if_none: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpCheck {
    pub method: Vec<u8>,
    pub uri: Vec<u8>,
    pub version: Vec<u8>,
    pub host: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OptionState<T> {
    Enabled(T),
    Disabled,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StatusRange {
    pub start: u16,
    pub end: u16,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RetryOnTrigger {
    ConnFailure,
    EmptyResponse,
    ResponseTimeout,
    JunkResponse,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RetryOn {
    None,
    Rules {
        triggers: Vec<RetryOnTrigger>,
        response_statuses: Vec<u16>,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Timeouts {
    pub client: Option<EffectiveValue<Duration>>,
    pub connect: Option<EffectiveValue<Duration>>,
    pub queue: Option<EffectiveValue<Duration>>,
    pub server: Option<EffectiveValue<Duration>>,
    pub http_request: Option<EffectiveValue<Duration>>,
    pub http_keep_alive: Option<EffectiveValue<Duration>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProxySettings {
    pub mode: Option<EffectiveValue<ProxyMode>>,
    pub default_backend: Option<EffectiveValue<BackendReference>>,
    pub balance: Option<EffectiveValue<BalanceAlgorithm>>,
    pub retries: Option<EffectiveValue<u32>>,
    pub retry_on: Option<EffectiveValue<RetryOn>>,
    pub redispatch: Option<EffectiveValue<OptionState<Redispatch>>>,
    pub timeouts: Timeouts,
    pub forward_for: Option<EffectiveValue<OptionState<ForwardFor>>>,
    pub http_check: Option<EffectiveValue<OptionState<HttpCheck>>>,
    pub http_check_send: Option<EffectiveValue<HttpCheck>>,
    pub http_check_expect: Option<EffectiveValue<Vec<StatusRange>>>,
    pub http_server_close: Option<EffectiveValue<bool>>,
    pub maxconn: Option<EffectiveValue<u64>>,
    pub http_request_rules: Vec<EffectiveValue<HttpRequestRule>>,
    pub http_response_rules: Vec<EffectiveValue<HttpResponseRule>>,
    pub semantic_blockers: Vec<EffectiveValue<SemanticBlocker>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StatsAdminPolicy {
    Localhost,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct StatsSettings {
    pub enable: Option<EffectiveValue<bool>>,
    pub uri_prefix: Option<EffectiveValue<Vec<u8>>>,
    pub refresh: Option<EffectiveValue<Duration>>,
    pub admin: Option<EffectiveValue<StatsAdminPolicy>>,
}

impl ProxySettings {
    fn inherited(&self, step: &InheritanceStep) -> Self {
        let mut inherited = self.clone();
        inherit_value(&mut inherited.mode, step);
        inherit_value(&mut inherited.default_backend, step);
        inherit_value(&mut inherited.balance, step);
        inherit_value(&mut inherited.retries, step);
        inherit_value(&mut inherited.retry_on, step);
        inherit_value(&mut inherited.redispatch, step);
        inherit_value(&mut inherited.timeouts.client, step);
        inherit_value(&mut inherited.timeouts.connect, step);
        inherit_value(&mut inherited.timeouts.queue, step);
        inherit_value(&mut inherited.timeouts.server, step);
        inherit_value(&mut inherited.timeouts.http_request, step);
        inherit_value(&mut inherited.timeouts.http_keep_alive, step);
        inherit_value(&mut inherited.forward_for, step);
        inherit_value(&mut inherited.http_check, step);
        inherit_value(&mut inherited.http_check_send, step);
        inherit_value(&mut inherited.http_check_expect, step);
        inherit_value(&mut inherited.http_server_close, step);
        inherit_value(&mut inherited.maxconn, step);
        for rule in &mut inherited.http_request_rules {
            rule.provenance.inherit(step.clone());
        }
        for rule in &mut inherited.http_response_rules {
            rule.provenance.inherit(step.clone());
        }
        for blocker in &mut inherited.semantic_blockers {
            blocker.provenance.inherit(step.clone());
        }
        inherited
    }
}

fn inherit_value<T>(value: &mut Option<EffectiveValue<T>>, step: &InheritanceStep) {
    if let Some(value) = value {
        value.provenance.inherit(step.clone());
    }
}

fn inherit_server_defaults(
    mut defaults: Option<EffectiveServer>,
    step: &InheritanceStep,
) -> Option<EffectiveServer> {
    let defaults = defaults.as_mut()?;
    inherit_value(&mut defaults.check, step);
    inherit_value(&mut defaults.interval, step);
    inherit_value(&mut defaults.fast_interval, step);
    inherit_value(&mut defaults.down_interval, step);
    inherit_value(&mut defaults.rise, step);
    inherit_value(&mut defaults.fall, step);
    inherit_value(&mut defaults.max_connections, step);
    inherit_value(&mut defaults.observe, step);
    inherit_value(&mut defaults.error_limit, step);
    inherit_value(&mut defaults.on_error, step);
    inherit_value(&mut defaults.weight, step);
    for option in &mut defaults.unsupported_options {
        option.provenance.inherit(step.clone());
    }
    Some(defaults.clone())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BindAddress {
    Tcp { host: Vec<u8>, port: u16 },
    Unix { path: Vec<u8> },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TlsAlpn {
    H2,
    Http11,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TlsMinimumVersion {
    Tls12,
    Tls13,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindTls {
    pub certificate_chain_path: PathBuf,
    pub private_key_path: PathBuf,
    pub dns_names: Vec<String>,
    pub alpn: Vec<TlsAlpn>,
    pub minimum_version: TlsMinimumVersion,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveBind {
    pub address: EffectiveValue<BindAddress>,
    pub mode: Option<EffectiveValue<u16>>,
    pub maxconn: Option<EffectiveValue<u64>>,
    pub tls: Option<EffectiveValue<BindTls>>,
}

impl std::ops::Deref for EffectiveBind {
    type Target = EffectiveValue<BindAddress>;

    fn deref(&self) -> &Self::Target {
        &self.address
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ServerAddress {
    Tcp { host: Vec<u8>, port: u16 },
    Unix { path: Vec<u8> },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveServer {
    pub name: EffectiveValue<Vec<u8>>,
    pub address: EffectiveValue<ServerAddress>,
    pub check: Option<EffectiveValue<bool>>,
    pub interval: Option<EffectiveValue<Duration>>,
    pub fast_interval: Option<EffectiveValue<Duration>>,
    pub down_interval: Option<EffectiveValue<Duration>>,
    pub rise: Option<EffectiveValue<u32>>,
    pub fall: Option<EffectiveValue<u32>>,
    pub max_connections: Option<EffectiveValue<u64>>,
    pub observe: Option<EffectiveValue<PassiveObserve>>,
    pub error_limit: Option<EffectiveValue<u32>>,
    pub on_error: Option<EffectiveValue<PassiveOnError>>,
    pub weight: Option<EffectiveValue<u16>>,
    pub unsupported_options: Vec<EffectiveValue<ServerOption>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerOption {
    pub name: Vec<u8>,
    pub arguments: Vec<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AclCriterion {
    HostExact,
    PathExact,
    PathPrefix,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AclDefinition {
    pub name: Vec<u8>,
    pub criterion: AclCriterion,
    pub case_insensitive: bool,
    pub values: Vec<Vec<u8>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AclReference {
    pub name: Vec<u8>,
    pub definitions: Vec<OccurrenceId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConditionPolarity {
    If,
    Unless,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UseBackend {
    pub backend: BackendReference,
    pub conditions: Vec<AclReference>,
    pub polarity: ConditionPolarity,
    pub condition_negated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HttpHeaderValue {
    Literal(Vec<u8>),
    ClientIp,
    IncomingAuthority,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HttpRequestRule {
    SetHeader {
        name: Vec<u8>,
        value: HttpHeaderValue,
    },
    RemoveHeader {
        name: Vec<u8>,
    },
    Redirect {
        status: u16,
        location: Vec<u8>,
    },
    FixedResponse {
        status: u16,
        body: Vec<u8>,
        content_type: Option<Vec<u8>>,
        condition: Option<HttpRequestCondition>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HttpRequestCondition {
    pub condition: AclReference,
    pub polarity: ConditionPolarity,
    pub condition_negated: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum HttpResponseRule {
    SetHeader { name: Vec<u8>, value: Vec<u8> },
    RemoveHeader { name: Vec<u8> },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EffectiveGlobal {
    pub sections: Vec<EffectiveSection>,
    pub maxconn: Option<EffectiveValue<u64>>,
    pub semantic_blockers: Vec<EffectiveValue<SemanticBlocker>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveDefaults {
    pub section: EffectiveSection,
    pub defaults: Option<DefaultsSource>,
    pub settings: ProxySettings,
    pub server_defaults: Option<EffectiveServer>,
    pub acls: Vec<EffectiveValue<AclDefinition>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveFrontend {
    pub section: EffectiveSection,
    pub defaults: Option<DefaultsSource>,
    pub settings: ProxySettings,
    pub binds: Vec<EffectiveBind>,
    pub acls: Vec<EffectiveValue<AclDefinition>>,
    pub use_backends: Vec<EffectiveValue<UseBackend>>,
    pub stats: StatsSettings,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveBackend {
    pub section: EffectiveSection,
    pub defaults: Option<DefaultsSource>,
    pub settings: ProxySettings,
    pub servers: Vec<EffectiveServer>,
    pub acls: Vec<EffectiveValue<AclDefinition>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EffectiveListen {
    pub section: EffectiveSection,
    pub defaults: Option<DefaultsSource>,
    pub settings: ProxySettings,
    pub binds: Vec<EffectiveBind>,
    pub servers: Vec<EffectiveServer>,
    pub acls: Vec<EffectiveValue<AclDefinition>>,
    pub use_backends: Vec<EffectiveValue<UseBackend>>,
    pub stats: StatsSettings,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Consumption {
    Section,
    Setting,
    Entry,
    Reference,
    Inheritance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BlockingReason {
    ConditionalPreprocessing,
    DuplicateIdentity,
    EnvironmentPreprocessing,
    Logging,
    Statistics,
    UnknownDirective,
    UnconsumedDirective,
    UnresolvedReference,
    UnsupportedForm,
    UnsupportedSection,
    ConflictingDirective,
    SemanticBlocker,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Externalization {
    ProcessOwned,
    LogTransport,
    Activation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DecisionOutcome {
    Consumed(Consumption),
    Superseded { by: OccurrenceId },
    Blocked(BlockingReason),
    Externalized(Externalization),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Decision {
    pub occurrence: OccurrenceId,
    pub section: Option<SectionId>,
    pub keyword: Vec<u8>,
    pub span: Span,
    pub outcome: DecisionOutcome,
}

/// Source-ordered, one-entry-per-occurrence terminal accounting.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct DecisionLedger {
    pub entries: Vec<Decision>,
}

impl DecisionLedger {
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Decision> {
        self.entries.iter()
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EffectiveConfiguration {
    pub global: EffectiveGlobal,
    pub defaults: Vec<EffectiveDefaults>,
    pub frontends: Vec<EffectiveFrontend>,
    pub backends: Vec<EffectiveBackend>,
    pub listens: Vec<EffectiveListen>,
    pub ledger: DecisionLedger,
    pub root_decisions: Vec<super::RootLoadDecision>,
    pub deployment_requirements: Vec<DeploymentRequirement<ProvenanceSpan>>,
    pub activation_requirements: Vec<ActivationRequirement<ProvenanceSpan>>,
    pub activation_only_sections: HashSet<SectionId>,
    pub supported_stats_sections: HashSet<SectionId>,
    pub blocked_stats_page_sections: HashSet<SectionId>,
}
