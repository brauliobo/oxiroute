use std::{
    collections::{HashMap, HashSet},
    fs::{File, Metadata},
    io::{BufReader, Read},
    net::IpAddr,
    path::PathBuf,
    time::{Duration, SystemTime},
};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use openssl::{
    pkey::{Id, PKey},
    x509::X509,
};
use oxiroute_config::{PassiveObserve, PassiveOnError};
use rustls_pemfile::{Item, read_one};
use x509_parser::{extensions::GeneralName, parse_x509_certificate};
use zeroize::Zeroizing;

use crate::{
    ActivationRequirement, ActivationRequirementKind, DeploymentRequirement,
    DeploymentRequirementKind, Diagnostic, DiagnosticCode, DiagnosticStage, ProvenanceRole,
    ProvenanceSpan, Report, Severity, SourceId, Span,
};
pub use crate::{E_DUPLICATE_IDENTITY, E_UNRESOLVED_REFERENCE};

use super::{
    Configuration, Directive, E_CONDITIONAL_PREPROCESSING, E_ENVIRONMENT_EXPANSION, Section,
    SectionKind,
};

mod http_directives;
mod server;

use http_directives::{
    parse_acl, parse_forward_for, parse_http_check, parse_http_check_send, parse_http_request_rule,
    parse_http_response_rule, parse_status_ranges,
};
use server::{merge_server_defaults, parse_server};

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

/// Resolves parsed `HAProxy` sources in their existing occurrence order.
#[must_use]
pub(super) fn resolve(configuration: &Configuration) -> Report<EffectiveConfiguration> {
    Resolver::new(configuration).run()
}

/// Resolves a parsed report without discarding source, lexing, or parsing diagnostics.
#[must_use]
pub(super) fn resolve_report(parsed: Report<Configuration>) -> Report<EffectiveConfiguration> {
    let (configuration, mut diagnostics) = parsed.into_parts();
    let (effective, resolve_diagnostics) = resolve(&configuration).into_parts();
    for diagnostic in resolve_diagnostics {
        if !diagnostics.contains(&diagnostic) {
            diagnostics.push(diagnostic);
        }
    }
    Report::new(effective, diagnostics)
}

#[derive(Clone)]
struct ParsedHeader {
    name: Option<(Vec<u8>, Span)>,
    from: Option<(Vec<u8>, Span)>,
}

#[derive(Clone)]
struct SectionMeta {
    id: SectionId,
    section: Section,
    header: Option<ParsedHeader>,
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DefaultsResolutionState {
    Unvisited,
    Visiting,
    Resolved,
}

struct PendingDecision {
    occurrence: OccurrenceId,
    section: Option<SectionId>,
    keyword: Vec<u8>,
    span: Span,
    outcome: Option<DecisionOutcome>,
}

#[derive(Default)]
struct SectionState {
    settings: ProxySettings,
    binds: Vec<EffectiveBind>,
    servers: Vec<EffectiveServer>,
    server_defaults: Option<EffectiveServer>,
    acls: Vec<EffectiveValue<AclDefinition>>,
    pending_http_request_rules: Vec<PendingHttpRequestRule>,
    pending_use_backends: Vec<PendingUseBackend>,
    use_backends: Vec<EffectiveValue<UseBackend>>,
    stats: StatsSettings,
}

struct PendingHttpRequestRule {
    occurrence: OccurrenceId,
    span: Span,
    rule: HttpRequestRule,
    condition: Option<PendingAclCondition>,
}

struct PendingAclCondition {
    name: Vec<u8>,
    span: Span,
    polarity: ConditionPolarity,
    negated: bool,
}

struct PendingUseBackend {
    occurrence: OccurrenceId,
    span: Span,
    backend_name: Vec<u8>,
    backend_span: Span,
    acl_conditions: Vec<PendingAclCondition>,
    polarity: ConditionPolarity,
    condition_negated: bool,
}

struct Resolver {
    preamble: Vec<(OccurrenceId, Directive)>,
    sections: Vec<SectionMeta>,
    decisions: Vec<PendingDecision>,
    decision_indices: HashMap<OccurrenceId, usize>,
    diagnostics: Vec<Diagnostic>,
    defaults_by_name: HashMap<Vec<u8>, Vec<usize>>,
    backends_by_name: HashMap<Vec<u8>, Vec<usize>>,
    defaults_state: Vec<DefaultsResolutionState>,
    resolved_defaults: Vec<Option<EffectiveDefaults>>,
    effective: EffectiveConfiguration,
}

impl Resolver {
    fn new(configuration: &Configuration) -> Self {
        let mut preamble = Vec::new();
        let mut sections = Vec::new();
        let mut decisions = Vec::new();
        let mut decision_indices = HashMap::new();

        for file in &configuration.files {
            let source = file.source.id();
            for (directive_ordinal, directive) in file.document.preamble.iter().enumerate() {
                let occurrence = OccurrenceId::Preamble {
                    source,
                    directive_ordinal,
                };
                push_pending_decision(
                    &mut decisions,
                    &mut decision_indices,
                    occurrence,
                    None,
                    directive,
                );
                preamble.push((occurrence, directive.clone()));
            }
            for (section_ordinal, section) in file.document.sections.iter().enumerate() {
                let id = SectionId {
                    source,
                    section_ordinal,
                };
                push_pending_decision(
                    &mut decisions,
                    &mut decision_indices,
                    OccurrenceId::SectionHeader(id),
                    Some(id),
                    &section.header,
                );
                for (directive_ordinal, directive) in section.directives.iter().enumerate() {
                    push_pending_decision(
                        &mut decisions,
                        &mut decision_indices,
                        OccurrenceId::SectionDirective {
                            section: id,
                            directive_ordinal,
                        },
                        Some(id),
                        directive,
                    );
                }
                sections.push(SectionMeta {
                    id,
                    section: section.clone(),
                    header: None,
                });
            }
        }

        let section_count = sections.len();
        Self {
            preamble,
            sections,
            decisions,
            decision_indices,
            diagnostics: Vec::new(),
            defaults_by_name: HashMap::new(),
            backends_by_name: HashMap::new(),
            defaults_state: vec![DefaultsResolutionState::Unvisited; section_count],
            resolved_defaults: vec![None; section_count],
            effective: EffectiveConfiguration {
                root_decisions: configuration.root_decisions.clone(),
                ..EffectiveConfiguration::default()
            },
        }
    }

    fn run(mut self) -> Report<EffectiveConfiguration> {
        self.prepare_headers_and_identities();
        self.resolve_preamble();

        for index in 0..self.sections.len() {
            match self.sections[index].section.kind {
                SectionKind::Global => self.resolve_global(index),
                SectionKind::Defaults => {
                    self.resolve_defaults(index);
                }
                SectionKind::Frontend | SectionKind::Backend | SectionKind::Listen => {
                    self.resolve_proxy(index);
                }
                _ => self.reject_unsupported_section(index),
            }
        }

        self.effective.defaults = self
            .sections
            .iter()
            .enumerate()
            .filter(|(_, section)| section.section.kind == SectionKind::Defaults)
            .filter_map(|(index, _)| self.resolved_defaults[index].clone())
            .collect();
        self.finish_ledger();

        Report::new(self.effective, self.diagnostics)
    }

    fn prepare_headers_and_identities(&mut self) {
        for index in 0..self.sections.len() {
            let meta = self.sections[index].clone();
            let occurrence = OccurrenceId::SectionHeader(meta.id);
            if self.block_environment(occurrence, &meta.section.header) {
                continue;
            }
            if !is_supported_section(meta.section.kind) {
                continue;
            }
            match parse_header(meta.section.kind, &meta.section.header) {
                Ok(header) => self.sections[index].header = Some(header),
                Err(message) => self.unsupported_form(
                    occurrence,
                    meta.section.header.span,
                    format!(
                        "unsupported HAProxy {} section header: {message}",
                        section_name(meta.section.kind)
                    ),
                ),
            }
        }

        let mut defaults_seen: HashMap<Vec<u8>, usize> = HashMap::new();
        let mut frontend_seen: HashMap<Vec<u8>, usize> = HashMap::new();
        let mut backend_seen: HashMap<Vec<u8>, usize> = HashMap::new();

        for index in 0..self.sections.len() {
            let Some(header) = self.sections[index].header.clone() else {
                continue;
            };
            let Some((name, _)) = &header.name else {
                continue;
            };
            let kind = self.sections[index].section.kind;
            if kind == SectionKind::Defaults {
                self.defaults_by_name
                    .entry(name.clone())
                    .or_default()
                    .push(index);
                self.register_identity(&mut defaults_seen, index, name, "defaults");
            }
            if matches!(kind, SectionKind::Frontend | SectionKind::Listen) {
                self.register_identity(&mut frontend_seen, index, name, "frontend");
            }
            if matches!(kind, SectionKind::Backend | SectionKind::Listen) {
                self.backends_by_name
                    .entry(name.clone())
                    .or_default()
                    .push(index);
                self.register_identity(&mut backend_seen, index, name, "backend");
            }
        }
    }

    fn register_identity(
        &mut self,
        seen: &mut HashMap<Vec<u8>, usize>,
        index: usize,
        name: &[u8],
        namespace: &str,
    ) {
        let Some(previous) = seen.insert(name.to_vec(), index) else {
            return;
        };
        let current_id = self.sections[index].id;
        let current_span = self.sections[index].section.header.span;
        let first_span = self.sections[previous].section.header.span;
        let occurrence = OccurrenceId::SectionHeader(current_id);
        self.block(occurrence, BlockingReason::DuplicateIdentity);
        self.diagnostics.push(
            Diagnostic::new(
                E_DUPLICATE_IDENTITY,
                Severity::Error,
                DiagnosticStage::Resolve,
                format!(
                    "duplicate HAProxy {namespace} identity `{}` cannot be represented uniquely",
                    display_bytes(name)
                ),
            )
            .with_primary_span(current_span)
            .with_related_span(first_span, "first declaration is here"),
        );
    }

    fn resolve_preamble(&mut self) {
        for (occurrence, directive) in self.preamble.clone() {
            if self.block_preprocessing(occurrence, &directive) {
                continue;
            }
            self.unknown_directive(occurrence, &directive, "before any section");
        }
    }

    fn resolve_global(&mut self, index: usize) {
        let meta = self.sections[index].clone();
        let Some(header) = meta.header.clone() else {
            self.block_section_directives(index, BlockingReason::UnsupportedForm);
            return;
        };
        debug_assert!(header.name.is_none());
        self.consume(OccurrenceId::SectionHeader(meta.id), Consumption::Section);
        self.effective
            .global
            .sections
            .push(effective_section(&meta, &header));

        for (directive_ordinal, directive) in meta.section.directives.iter().enumerate() {
            let occurrence = section_directive_id(meta.id, directive_ordinal);
            if self.block_preprocessing(occurrence, directive) {
                continue;
            }
            match directive.name.value.as_slice() {
                b"maxconn" => {
                    let Some(value) = parse_one_u64(directive) else {
                        self.unsupported_directive_form(occurrence, directive, SectionKind::Global);
                        continue;
                    };
                    let value =
                        EffectiveValue::direct(value, occurrence, directive.arguments[0].span);
                    let conflict = set_value(
                        &mut self.effective.global.maxconn,
                        value,
                        &mut self.decisions,
                        &self.decision_indices,
                    );
                    if let Some(first_span) = conflict {
                        self.conflicting_directive(occurrence, directive, first_span);
                        self.effective
                            .global
                            .semantic_blockers
                            .push(semantic_blocker(
                                SemanticBlockerKind::ConflictingDirective,
                                occurrence,
                                directive,
                            ));
                    } else {
                        self.consume(occurrence, Consumption::Setting);
                    }
                }
                b"stats" => self.externalize_activation(
                    occurrence,
                    directive,
                    ActivationRequirementKind::StatisticsEndpoint,
                    None,
                    false,
                ),
                name if is_logging_directive_name(name) => {
                    self.externalize_log_transport(occurrence, directive);
                }
                name if is_global_security_directive(name) => {
                    self.effective
                        .global
                        .semantic_blockers
                        .push(semantic_blocker(
                            SemanticBlockerKind::GlobalSecurity,
                            occurrence,
                            directive,
                        ));
                    self.reject_semantic_directive(
                        occurrence,
                        directive,
                        "HAProxy global TLS or security policy is not represented by the canonical configuration",
                    );
                }
                name if is_process_owned(name) => {
                    self.externalize_process_setting(occurrence, directive);
                }
                _ => self.unknown_directive(occurrence, directive, "in a global section"),
            }
        }
    }

    fn resolve_defaults(&mut self, index: usize) -> Option<EffectiveDefaults> {
        match self
            .defaults_state
            .get(index)
            .copied()
            .expect("section was indexed")
        {
            DefaultsResolutionState::Resolved => return self.resolved_defaults[index].clone(),
            DefaultsResolutionState::Visiting => return None,
            DefaultsResolutionState::Unvisited => {}
        }
        self.defaults_state[index] = DefaultsResolutionState::Visiting;

        let meta = self.sections[index].clone();
        let Some(header) = meta.header.clone() else {
            self.block_section_directives(index, BlockingReason::UnsupportedForm);
            self.defaults_state[index] = DefaultsResolutionState::Resolved;
            return None;
        };

        let (settings, server_defaults, defaults) = self.explicit_defaults_base(&meta, &header);
        let mut state = SectionState {
            settings,
            server_defaults,
            ..SectionState::default()
        };
        self.resolve_section_directives(index, &header, &mut state);
        self.finish_http_request_rules(&mut state);
        self.finish_use_backends(&mut state);

        let resolved = EffectiveDefaults {
            section: effective_section(&meta, &header),
            defaults,
            settings: state.settings,
            server_defaults: state.server_defaults,
            acls: state.acls,
        };
        self.consume(
            OccurrenceId::SectionHeader(meta.id),
            if resolved.defaults.is_some() {
                Consumption::Inheritance
            } else {
                Consumption::Section
            },
        );
        self.defaults_state[index] = DefaultsResolutionState::Resolved;
        self.resolved_defaults[index] = Some(resolved.clone());
        Some(resolved)
    }

    fn explicit_defaults_base(
        &mut self,
        meta: &SectionMeta,
        header: &ParsedHeader,
    ) -> (
        ProxySettings,
        Option<EffectiveServer>,
        Option<DefaultsSource>,
    ) {
        let Some((name, reference_span)) = &header.from else {
            return (ProxySettings::default(), None, None);
        };
        let Some(target_index) = self.resolve_defaults_reference(
            OccurrenceId::SectionHeader(meta.id),
            *reference_span,
            name,
        ) else {
            return (ProxySettings::default(), None, None);
        };
        if self.defaults_state[target_index] == DefaultsResolutionState::Visiting {
            self.unresolved_reference(
                OccurrenceId::SectionHeader(meta.id),
                *reference_span,
                "defaults",
                name,
                &[],
                "forms an inheritance cycle",
            );
            return (ProxySettings::default(), None, None);
        }
        let Some(target) = self.resolve_defaults(target_index) else {
            self.unresolved_reference(
                OccurrenceId::SectionHeader(meta.id),
                *reference_span,
                "defaults",
                name,
                &[],
                "does not resolve to a representable defaults section",
            );
            return (ProxySettings::default(), None, None);
        };
        let step = InheritanceStep {
            source_defaults: target.section.id,
            destination: meta.id,
            selection: DefaultsSelection::Explicit,
            reference_span: Some(*reference_span),
        };
        let source = defaults_source(
            meta,
            target.section.id,
            target.section.declaration,
            target.section.span,
            DefaultsSelection::Explicit,
            *reference_span,
        );
        (
            target.settings.inherited(&step),
            inherit_server_defaults(target.server_defaults, &step),
            Some(source),
        )
    }

    fn resolve_proxy(&mut self, index: usize) {
        let meta = self.sections[index].clone();
        let Some(header) = meta.header.clone() else {
            self.block_section_directives(index, BlockingReason::UnsupportedForm);
            return;
        };
        let (settings, server_defaults, defaults) = self.proxy_defaults_base(index, &meta, &header);
        let mut state = SectionState {
            settings,
            server_defaults,
            ..SectionState::default()
        };
        self.resolve_section_directives(index, &header, &mut state);
        self.finish_http_request_rules(&mut state);
        self.finish_use_backends(&mut state);
        self.consume(
            OccurrenceId::SectionHeader(meta.id),
            if defaults.is_some() {
                Consumption::Inheritance
            } else {
                Consumption::Section
            },
        );

        let section = effective_section(&meta, &header);
        match meta.section.kind {
            SectionKind::Frontend => self.effective.frontends.push(EffectiveFrontend {
                section,
                defaults,
                settings: state.settings,
                binds: state.binds,
                acls: state.acls,
                use_backends: state.use_backends,
                stats: state.stats,
            }),
            SectionKind::Backend => self.effective.backends.push(EffectiveBackend {
                section,
                defaults,
                settings: state.settings,
                servers: state.servers,
                acls: state.acls,
            }),
            SectionKind::Listen => self.effective.listens.push(EffectiveListen {
                section,
                defaults,
                settings: state.settings,
                binds: state.binds,
                servers: state.servers,
                acls: state.acls,
                use_backends: state.use_backends,
                stats: state.stats,
            }),
            _ => unreachable!("caller selected a proxy section"),
        }
    }

    fn proxy_defaults_base(
        &mut self,
        index: usize,
        meta: &SectionMeta,
        header: &ParsedHeader,
    ) -> (
        ProxySettings,
        Option<EffectiveServer>,
        Option<DefaultsSource>,
    ) {
        let (target_index, selection, reference_span) = if let Some((name, span)) = &header.from {
            let Some(target) =
                self.resolve_defaults_reference(OccurrenceId::SectionHeader(meta.id), *span, name)
            else {
                return (ProxySettings::default(), None, None);
            };
            (target, DefaultsSelection::Explicit, *span)
        } else {
            let Some(target) = self.sections[..index]
                .iter()
                .rposition(|section| section.section.kind == SectionKind::Defaults)
            else {
                return (ProxySettings::default(), None, None);
            };
            (
                target,
                DefaultsSelection::ImplicitLatest,
                meta.section.header.span,
            )
        };
        let Some(target) = self.resolve_defaults(target_index) else {
            self.unresolved_reference(
                OccurrenceId::SectionHeader(meta.id),
                reference_span,
                "defaults",
                header
                    .from
                    .as_ref()
                    .map_or(b"<latest>".as_slice(), |(name, _)| name.as_slice()),
                &[],
                "does not resolve to a representable defaults section",
            );
            return (ProxySettings::default(), None, None);
        };
        let step = InheritanceStep {
            source_defaults: target.section.id,
            destination: meta.id,
            selection,
            reference_span: (selection == DefaultsSelection::Explicit).then_some(reference_span),
        };
        let source = defaults_source(
            meta,
            target.section.id,
            target.section.declaration,
            target.section.span,
            selection,
            reference_span,
        );
        (
            target.settings.inherited(&step),
            inherit_server_defaults(target.server_defaults, &step),
            Some(source),
        )
    }

    #[allow(clippy::too_many_lines)]
    fn resolve_section_directives(
        &mut self,
        index: usize,
        header: &ParsedHeader,
        state: &mut SectionState,
    ) {
        let meta = self.sections[index].clone();
        for (directive_ordinal, directive) in meta.section.directives.iter().enumerate() {
            let occurrence = section_directive_id(meta.id, directive_ordinal);
            if self.block_preprocessing(occurrence, directive) {
                continue;
            }
            if directive.name.value == b"http-request"
                && directive
                    .arguments
                    .first()
                    .is_some_and(|argument| argument.value == b"use-service")
                && directive
                    .arguments
                    .get(1)
                    .is_some_and(|argument| argument.value == b"prometheus-exporter")
            {
                let supported = exact_prometheus_exporter(directive);
                self.externalize_activation(
                    occurrence,
                    directive,
                    ActivationRequirementKind::PrometheusExporter,
                    Some(meta.id),
                    supported,
                );
                continue;
            }
            if directive.name.value == b"stats" {
                if supports_stats_page(meta.section.kind)
                    && self.resolve_stats(occurrence, directive, state)
                {
                    self.effective.activation_only_sections.insert(meta.id);
                } else {
                    self.effective.blocked_stats_page_sections.insert(meta.id);
                    self.externalize_activation(
                        occurrence,
                        directive,
                        ActivationRequirementKind::StatisticsEndpoint,
                        Some(meta.id),
                        false,
                    );
                }
                continue;
            }
            if is_logging_directive(directive) {
                self.externalize_log_transport(occurrence, directive);
                continue;
            }
            if is_process_owned(&directive.name.value) {
                self.externalize_process_setting(occurrence, directive);
                continue;
            }

            match directive.name.value.as_slice() {
                b"mode" => self.resolve_mode(occurrence, directive, state),
                b"bind" if supports_bind(meta.section.kind) => {
                    self.resolve_bind(occurrence, directive, state);
                }
                b"default_backend" if supports_default_backend(meta.section.kind) => {
                    self.resolve_default_backend(occurrence, directive, state);
                }
                b"balance" if supports_balance(meta.section.kind) => {
                    self.resolve_balance(occurrence, directive, state);
                }
                b"server" if supports_server(meta.section.kind) => {
                    self.resolve_server(occurrence, directive, state);
                }
                b"default-server" if supports_backend_policy(meta.section.kind) => {
                    self.resolve_default_server(occurrence, directive, state);
                }
                b"retries" if supports_backend_policy(meta.section.kind) => {
                    self.resolve_retries(occurrence, directive, state);
                }
                b"timeout" => self.resolve_timeout(occurrence, directive, state),
                b"maxconn" if supports_maxconn(meta.section.kind) => {
                    self.resolve_maxconn(occurrence, directive, state);
                }
                b"acl" => self.resolve_acl(occurrence, directive, header, state),
                b"use_backend" if supports_use_backend(meta.section.kind) => {
                    self.resolve_use_backend(occurrence, directive, state);
                }
                b"http-request" if supports_http_rules(meta.section.kind) => {
                    self.resolve_http_request_rule(occurrence, directive, state);
                }
                b"http-response" if supports_http_rules(meta.section.kind) => {
                    self.resolve_http_response_rule(occurrence, directive, state);
                }
                b"option" | b"no" => {
                    self.resolve_option(occurrence, directive, meta.section.kind, state);
                }
                b"http-check" if supports_backend_policy(meta.section.kind) => {
                    self.resolve_http_check(occurrence, directive, state);
                }
                b"retry-on" if supports_backend_policy(meta.section.kind) => {
                    self.resolve_retry_on(occurrence, directive, state);
                }
                name if is_proxy_default_directive(name) => {
                    self.track_and_reject_semantics(
                        occurrence,
                        directive,
                        SemanticBlockerKind::ProxyDefault,
                        state,
                        "HAProxy proxy defaults or dispatch policy are not represented by the import IR",
                    );
                }
                name if is_known_resolver_directive(name) => {
                    self.unsupported_directive_form(occurrence, directive, meta.section.kind);
                }
                _ => self.unknown_directive(
                    occurrence,
                    directive,
                    &format!("in a {} section", section_name(meta.section.kind)),
                ),
            }
        }
    }

    fn resolve_stats(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) -> bool {
        match directive.arguments.as_slice() {
            [enable] if enable.value == b"enable" => {
                let value = EffectiveValue::direct(true, occurrence, enable.span);
                let conflict = set_idempotent_value(
                    &mut state.stats.enable,
                    value,
                    &mut self.decisions,
                    &self.decision_indices,
                );
                if let Err(first_span) = conflict {
                    self.conflicting_directive(occurrence, directive, first_span);
                } else {
                    self.consume(occurrence, Consumption::Setting);
                }
                true
            }
            [uri, prefix] if uri.value == b"uri" && !prefix.value.is_empty() => {
                let value = EffectiveValue::direct(prefix.value.clone(), occurrence, prefix.span);
                let conflict = set_idempotent_value(
                    &mut state.stats.uri_prefix,
                    value,
                    &mut self.decisions,
                    &self.decision_indices,
                );
                if let Err(first_span) = conflict {
                    self.conflicting_directive(occurrence, directive, first_span);
                } else {
                    state.stats.enable.get_or_insert_with(|| {
                        EffectiveValue::direct(true, occurrence, prefix.span)
                    });
                    self.consume(occurrence, Consumption::Setting);
                }
                true
            }
            [refresh, raw] if refresh.value == b"refresh" => {
                let Some(duration) = parse_duration(&raw.value) else {
                    return false;
                };
                let value = EffectiveValue::direct(duration, occurrence, raw.span);
                let conflict = set_idempotent_value(
                    &mut state.stats.refresh,
                    value,
                    &mut self.decisions,
                    &self.decision_indices,
                );
                if let Err(first_span) = conflict {
                    self.conflicting_directive(occurrence, directive, first_span);
                } else {
                    self.consume(occurrence, Consumption::Setting);
                }
                true
            }
            [admin, condition, localhost]
                if admin.value == b"admin"
                    && condition.value == b"if"
                    && localhost.value == b"LOCALHOST" =>
            {
                let value =
                    EffectiveValue::direct(StatsAdminPolicy::Localhost, occurrence, localhost.span);
                let conflict = set_idempotent_value(
                    &mut state.stats.admin,
                    value,
                    &mut self.decisions,
                    &self.decision_indices,
                );
                if let Err(first_span) = conflict {
                    self.conflicting_directive(occurrence, directive, first_span);
                } else {
                    self.consume(occurrence, Consumption::Setting);
                }
                true
            }
            _ => false,
        }
    }

    fn resolve_mode(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let Some(argument) = exactly_one_argument(directive) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        let mode = match argument.value.as_slice() {
            b"http" => ProxyMode::Http,
            b"tcp" => ProxyMode::Tcp,
            _ => {
                let value = EffectiveValue::direct(
                    ProxyMode::Unsupported(argument.value.clone()),
                    occurrence,
                    argument.span,
                );
                let conflict = self.set_setting(&mut state.settings.mode, value);
                if let Some(first_span) = conflict {
                    self.conflicting_directive(occurrence, directive, first_span);
                } else {
                    self.block(occurrence, BlockingReason::SemanticBlocker);
                    self.diagnostics.push(
                        Diagnostic::new(
                            E_UNSUPPORTED_FORM,
                            Severity::Error,
                            DiagnosticStage::Resolve,
                            format!(
                                "unsupported HAProxy mode `{}` cannot inherit or lower as HTTP",
                                display_bytes(&argument.value)
                            ),
                        )
                        .with_primary_span(argument.span),
                    );
                }
                state.settings.semantic_blockers.push(semantic_blocker(
                    SemanticBlockerKind::Mode,
                    occurrence,
                    directive,
                ));
                return;
            }
        };
        let value = EffectiveValue::direct(mode, occurrence, argument.span);
        if let Some(first_span) = self.set_setting(&mut state.settings.mode, value) {
            self.conflicting_directive(occurrence, directive, first_span);
            state.settings.semantic_blockers.push(semantic_blocker(
                SemanticBlockerKind::ConflictingDirective,
                occurrence,
                directive,
            ));
        } else {
            self.consume(occurrence, Consumption::Setting);
        }
    }

    fn resolve_bind(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        match parse_bind(directive, occurrence) {
            Ok(binds) => {
                state.binds.extend(binds);
                self.consume(occurrence, Consumption::Entry);
            }
            Err(BindParseError::Malformed) => {
                self.unsupported_directive_form_for_occurrence(occurrence, directive);
            }
            Err(BindParseError::Semantic(message)) => {
                self.block_bind_semantics(occurrence, directive, state, &message);
            }
            Err(BindParseError::Conflict {
                name,
                current_span,
                previous_span,
            }) => {
                self.conflicting_option(occurrence, current_span, previous_span, &name);
                state.settings.semantic_blockers.push(semantic_blocker(
                    SemanticBlockerKind::ConflictingDirective,
                    occurrence,
                    directive,
                ));
            }
        }
    }

    fn resolve_default_backend(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let Some(argument) = exactly_one_argument(directive) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        let Some((target, reference_target)) =
            self.resolve_backend_reference(occurrence, argument.span, &argument.value)
        else {
            return;
        };
        let value = EffectiveValue::direct_reference(
            BackendReference {
                name: argument.value.clone(),
                target,
            },
            occurrence,
            argument.span,
            vec![reference_target],
        );
        let conflict = self.set_setting(&mut state.settings.default_backend, value);
        if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
            self.consume(occurrence, Consumption::Reference);
        }
    }

    fn resolve_balance(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let Some(argument) = exactly_one_argument(directive) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        let algorithm = match argument.value.as_slice() {
            b"roundrobin" => BalanceAlgorithm::RoundRobin,
            b"leastconn" => BalanceAlgorithm::LeastConnections,
            b"first" => BalanceAlgorithm::First,
            _ => {
                self.unsupported_directive_form_for_occurrence(occurrence, directive);
                return;
            }
        };
        let value = EffectiveValue::direct(algorithm, occurrence, argument.span);
        let conflict = self.set_setting(&mut state.settings.balance, value);
        if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
            self.consume(occurrence, Consumption::Setting);
        }
    }

    fn resolve_server(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let Some(parsed) = parse_server(directive, occurrence) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        let mut server = parsed.server;
        if let Some(defaults) = &state.server_defaults {
            if server.check.is_none() {
                server.check.clone_from(&defaults.check);
            }
            if server.interval.is_none() {
                server.interval.clone_from(&defaults.interval);
            }
            if server.fast_interval.is_none() {
                server.fast_interval.clone_from(&defaults.fast_interval);
            }
            if server.down_interval.is_none() {
                server.down_interval.clone_from(&defaults.down_interval);
            }
            if server.rise.is_none() {
                server.rise.clone_from(&defaults.rise);
            }
            if server.fall.is_none() {
                server.fall.clone_from(&defaults.fall);
            }
            if server.max_connections.is_none() {
                server.max_connections.clone_from(&defaults.max_connections);
            }
            if server.observe.is_none() {
                server.observe.clone_from(&defaults.observe);
            }
            if server.error_limit.is_none() {
                server.error_limit.clone_from(&defaults.error_limit);
            }
            if server.on_error.is_none() {
                server.on_error.clone_from(&defaults.on_error);
            }
            server
                .unsupported_options
                .extend(defaults.unsupported_options.iter().cloned());
        }
        for conflict in parsed.conflicts {
            self.conflicting_option(
                occurrence,
                conflict.current_span,
                conflict.previous_span,
                &conflict.name,
            );
        }
        if !server.unsupported_options.is_empty() {
            self.block(occurrence, BlockingReason::SemanticBlocker);
            self.diagnostics.push(
                Diagnostic::new(
                    E_UNSUPPORTED_FORM,
                    Severity::Error,
                    DiagnosticStage::Resolve,
                    format!(
                        "HAProxy server options `{}` affect selection, capacity, TLS, or health-check behavior that is not represented canonically",
                        server
                            .unsupported_options
                            .iter()
                            .map(|option| display_bytes(&option.value.name))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
                .with_primary_span(directive.span),
            );
        }
        if let Some(previous) = state
            .servers
            .iter()
            .find(|candidate| candidate.name.value == server.name.value)
        {
            self.block(occurrence, BlockingReason::DuplicateIdentity);
            self.diagnostics.push(
                Diagnostic::new(
                    E_DUPLICATE_IDENTITY,
                    Severity::Error,
                    DiagnosticStage::Resolve,
                    format!(
                        "duplicate HAProxy server identity `{}` cannot be represented uniquely",
                        display_bytes(&server.name.value)
                    ),
                )
                .with_primary_span(server.name.provenance.origin_span)
                .with_related_span(
                    previous.name.provenance.origin_span,
                    "first declaration is here",
                ),
            );
        } else if self.pending_decision_mut(occurrence).outcome.is_none() {
            self.consume(occurrence, Consumption::Entry);
        }
        state.servers.push(server);
    }

    fn resolve_default_server(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let synthetic_word = |value: &[u8]| super::Word {
            value: value.to_vec(),
            span: directive.span,
            environment_references: Vec::new(),
        };
        let mut synthetic = directive.clone();
        synthetic.arguments = vec![
            synthetic_word("__defaults".as_bytes()),
            synthetic_word("127.0.0.1:1".as_bytes()),
        ];
        synthetic.arguments.extend(directive.arguments.clone());
        let Some(parsed) = parse_server(&synthetic, occurrence) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        for conflict in parsed.conflicts {
            self.conflicting_option(
                occurrence,
                conflict.current_span,
                conflict.previous_span,
                &conflict.name,
            );
        }
        if parsed.server.unsupported_options.is_empty() {
            match &mut state.server_defaults {
                Some(defaults) => merge_server_defaults(defaults, parsed.server),
                None => state.server_defaults = Some(parsed.server),
            }
            self.consume(occurrence, Consumption::Setting);
        } else {
            self.track_and_reject_semantics(
                occurrence,
                directive,
                SemanticBlockerKind::ProxyDefault,
                state,
                "HAProxy default-server contains options without canonical server equivalents",
            );
        }
    }

    fn resolve_retries(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let Some(value) = parse_one_u32(directive) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        let value = EffectiveValue::direct(value, occurrence, directive.arguments[0].span);
        let conflict = self.set_setting(&mut state.settings.retries, value);
        if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
            self.consume(occurrence, Consumption::Setting);
        }
    }

    fn resolve_retry_on(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let retry_on = match parse_retry_on(&directive.arguments) {
            Ok(retry_on) => retry_on,
            Err(reason) => {
                let message = format!("unsupported HAProxy retry-on form: {reason}");
                self.track_and_reject_semantics(
                    occurrence,
                    directive,
                    SemanticBlockerKind::Retry,
                    state,
                    &message,
                );
                return;
            }
        };
        let value = EffectiveValue::direct(retry_on, occurrence, directive.span);
        let conflict = self.set_setting(&mut state.settings.retry_on, value);
        if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
            self.consume(occurrence, Consumption::Setting);
        }
    }

    fn resolve_timeout(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let [class, raw] = directive.arguments.as_slice() else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        let Some(duration) = parse_duration(&raw.value) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        let value = EffectiveValue::direct(duration, occurrence, raw.span);
        let slot = match class.value.as_slice() {
            b"client" => &mut state.settings.timeouts.client,
            b"connect" => &mut state.settings.timeouts.connect,
            b"queue" => &mut state.settings.timeouts.queue,
            b"server" => &mut state.settings.timeouts.server,
            b"http-request" => &mut state.settings.timeouts.http_request,
            b"http-keep-alive" => &mut state.settings.timeouts.http_keep_alive,
            _ => {
                self.track_and_reject_semantics(
                    occurrence,
                    directive,
                    SemanticBlockerKind::Timeout,
                    state,
                    "HAProxy timeout class has no canonical equivalent",
                );
                return;
            }
        };
        let conflict = set_value(slot, value, &mut self.decisions, &self.decision_indices);
        if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
            self.consume(occurrence, Consumption::Setting);
        }
    }

    fn resolve_maxconn(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let Some(value) = parse_one_u64(directive) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        let value = EffectiveValue::direct(value, occurrence, directive.arguments[0].span);
        let conflict = self.set_setting(&mut state.settings.maxconn, value);
        if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
            self.consume(occurrence, Consumption::Setting);
        }
    }

    fn resolve_http_request_rule(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let Some((rule, condition)) = parse_http_request_rule(&directive.arguments) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        state
            .pending_http_request_rules
            .push(PendingHttpRequestRule {
                occurrence,
                span: directive.span,
                rule,
                condition,
            });
    }

    fn resolve_http_response_rule(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let Some(rule) = parse_http_response_rule(&directive.arguments) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        state
            .settings
            .http_response_rules
            .push(EffectiveValue::direct(rule, occurrence, directive.span));
        self.consume(occurrence, Consumption::Entry);
    }

    fn resolve_acl(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        header: &ParsedHeader,
        state: &mut SectionState,
    ) {
        if self.section_kind(occurrence) == Some(SectionKind::Defaults) && header.name.is_none() {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        }
        let Some(acl) = parse_acl(directive) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        state
            .acls
            .push(EffectiveValue::direct(acl, occurrence, directive.span));
        self.consume(occurrence, Consumption::Entry);
    }

    fn resolve_use_backend(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let [backend, polarity, rest @ ..] = directive.arguments.as_slice() else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        let polarity = match polarity.value.as_slice() {
            b"if" => ConditionPolarity::If,
            b"unless" => ConditionPolarity::Unless,
            _ => {
                self.unsupported_directive_form_for_occurrence(occurrence, directive);
                return;
            }
        };
        if rest
            .iter()
            .any(|word| matches!(word.value.as_slice(), b"{" | b"}"))
        {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        }
        if rest.is_empty() {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        }
        let mut condition_negated = false;
        let mut acl_conditions = Vec::new();
        let mut index = 0;
        while index < rest.len() {
            if rest[index].value == b"!" {
                condition_negated = true;
                index += 1;
            }
            let Some(acl) = rest.get(index) else {
                self.unsupported_directive_form_for_occurrence(occurrence, directive);
                return;
            };
            acl_conditions.push(PendingAclCondition {
                name: acl.value.clone(),
                span: acl.span,
                polarity,
                negated: false,
            });
            index += 1;
        }
        state.pending_use_backends.push(PendingUseBackend {
            occurrence,
            span: directive.span,
            backend_name: backend.value.clone(),
            backend_span: backend.span,
            acl_conditions,
            polarity,
            condition_negated,
        });
    }

    #[allow(clippy::too_many_lines)]
    fn resolve_option(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        kind: SectionKind,
        state: &mut SectionState,
    ) {
        let disabled = directive.name.value == b"no";
        let arguments = if disabled {
            let [option, rest @ ..] = directive.arguments.as_slice() else {
                self.unsupported_directive_form_for_occurrence(occurrence, directive);
                return;
            };
            if option.value != b"option" {
                self.unsupported_directive_form_for_occurrence(occurrence, directive);
                return;
            }
            rest
        } else {
            directive.arguments.as_slice()
        };
        let Some((name, arguments)) = arguments.split_first() else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };

        match name.value.as_slice() {
            b"redispatch" if supports_backend_policy(kind) => {
                let value = if disabled {
                    if !arguments.is_empty() {
                        self.unsupported_directive_form_for_occurrence(occurrence, directive);
                        return;
                    }
                    OptionState::Disabled
                } else {
                    let interval = match arguments {
                        [] => None,
                        [interval] => parse_i32(&interval.value),
                        _ => {
                            self.unsupported_directive_form_for_occurrence(occurrence, directive);
                            return;
                        }
                    };
                    if !arguments.is_empty() && interval.is_none() {
                        self.unsupported_directive_form_for_occurrence(occurrence, directive);
                        return;
                    }
                    OptionState::Enabled(Redispatch { interval })
                };
                let value = EffectiveValue::direct(value, occurrence, directive.span);
                let conflict = self.set_setting(&mut state.settings.redispatch, value);
                if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
                    self.consume(occurrence, Consumption::Setting);
                }
            }
            b"forwardfor" => {
                let value = if disabled {
                    if !arguments.is_empty() {
                        self.unsupported_directive_form_for_occurrence(occurrence, directive);
                        return;
                    }
                    OptionState::Disabled
                } else {
                    let Some(forward_for) = parse_forward_for(arguments) else {
                        self.unsupported_directive_form_for_occurrence(occurrence, directive);
                        return;
                    };
                    OptionState::Enabled(forward_for)
                };
                let value = EffectiveValue::direct(value, occurrence, directive.span);
                let conflict = self.set_setting(&mut state.settings.forward_for, value);
                if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
                    self.consume(occurrence, Consumption::Setting);
                }
            }
            b"httpchk" if supports_backend_policy(kind) => {
                let value = if disabled {
                    if !arguments.is_empty() {
                        self.unsupported_directive_form_for_occurrence(occurrence, directive);
                        return;
                    }
                    OptionState::Disabled
                } else {
                    let Some(check) = parse_http_check(arguments) else {
                        self.unsupported_directive_form_for_occurrence(occurrence, directive);
                        return;
                    };
                    OptionState::Enabled(check)
                };
                let value = EffectiveValue::direct(value, occurrence, directive.span);
                let conflict = self.set_setting(&mut state.settings.http_check, value);
                if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
                    self.consume(occurrence, Consumption::Setting);
                }
            }
            b"http-server-close" if supports_backend_policy(kind) => {
                if !arguments.is_empty() {
                    self.unsupported_directive_form_for_occurrence(occurrence, directive);
                    return;
                }
                let value = EffectiveValue::direct(!disabled, occurrence, directive.span);
                let conflict = self.set_setting(&mut state.settings.http_server_close, value);
                if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
                    self.consume(occurrence, Consumption::Setting);
                }
            }
            name if is_logging_option(name) => {
                self.externalize_log_transport(occurrence, directive);
            }
            _ => self.track_and_reject_semantics(
                occurrence,
                directive,
                SemanticBlockerKind::ProxyDefault,
                state,
                "HAProxy option changes proxy behavior that is not represented by the import IR",
            ),
        }
    }

    fn resolve_http_check_expect(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        let [expect, status, pattern] = directive.arguments.as_slice() else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        if expect.value != b"expect" || status.value != b"status" {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        }
        let Some(ranges) = parse_status_ranges(&pattern.value) else {
            self.unsupported_directive_form_for_occurrence(occurrence, directive);
            return;
        };
        let value = EffectiveValue::direct(ranges, occurrence, pattern.span);
        let conflict = self.set_setting(&mut state.settings.http_check_expect, value);
        if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
            self.consume(occurrence, Consumption::Setting);
        }
    }

    fn resolve_http_check(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
    ) {
        match directive
            .arguments
            .first()
            .map(|argument| argument.value.as_slice())
        {
            Some(b"expect") => self.resolve_http_check_expect(occurrence, directive, state),
            Some(b"send") => {
                let Some(check) = parse_http_check_send(&directive.arguments[1..]) else {
                    self.unsupported_directive_form_for_occurrence(occurrence, directive);
                    return;
                };
                let value = EffectiveValue::direct(check, occurrence, directive.span);
                let conflict = self.set_setting(&mut state.settings.http_check_send, value);
                if !self.finish_setting(occurrence, directive, conflict, &mut state.settings) {
                    self.consume(occurrence, Consumption::Setting);
                }
            }
            _ => self.unsupported_directive_form_for_occurrence(occurrence, directive),
        }
    }

    fn finish_http_request_rules(&mut self, state: &mut SectionState) {
        let mut definitions: HashMap<Vec<u8>, Vec<&EffectiveValue<AclDefinition>>> = HashMap::new();
        for acl in &state.acls {
            definitions
                .entry(acl.value.name.clone())
                .or_default()
                .push(acl);
        }

        for pending in state.pending_http_request_rules.drain(..) {
            let mut references = Vec::new();
            let condition = if let Some(condition) = pending.condition {
                let Some(acls) = definitions.get(&condition.name) else {
                    self.unresolved_reference(
                        pending.occurrence,
                        condition.span,
                        "ACL",
                        &condition.name,
                        &[],
                        "is not defined in this section",
                    );
                    continue;
                };
                let targets = acls
                    .iter()
                    .map(|acl| ReferenceTarget {
                        occurrence: acl.provenance.origin,
                        span: acl.provenance.origin_span,
                    })
                    .collect::<Vec<_>>();
                references.push(ReferenceProvenance {
                    use_span: condition.span,
                    targets: targets.clone(),
                });
                Some(HttpRequestCondition {
                    condition: AclReference {
                        name: condition.name,
                        definitions: targets.iter().map(|target| target.occurrence).collect(),
                    },
                    polarity: condition.polarity,
                    condition_negated: condition.negated,
                })
            } else {
                None
            };
            let mut rule = pending.rule;
            if let HttpRequestRule::FixedResponse {
                condition: target, ..
            } = &mut rule
            {
                *target = condition;
            }
            state
                .settings
                .http_request_rules
                .push(EffectiveValue::direct_references(
                    rule,
                    pending.occurrence,
                    pending.span,
                    references,
                ));
            self.consume(pending.occurrence, Consumption::Entry);
        }
    }

    fn finish_use_backends(&mut self, state: &mut SectionState) {
        let mut definitions: HashMap<Vec<u8>, Vec<&EffectiveValue<AclDefinition>>> = HashMap::new();
        for acl in &state.acls {
            definitions
                .entry(acl.value.name.clone())
                .or_default()
                .push(acl);
        }

        for pending in state.pending_use_backends.drain(..) {
            let Some((target, backend_target)) = self.resolve_backend_reference(
                pending.occurrence,
                pending.backend_span,
                &pending.backend_name,
            ) else {
                continue;
            };
            let mut conditions = Vec::with_capacity(pending.acl_conditions.len());
            let mut references = vec![ReferenceProvenance {
                use_span: pending.backend_span,
                targets: vec![backend_target],
            }];
            let mut unresolved = false;
            for condition in &pending.acl_conditions {
                let Some(acls) = definitions.get(&condition.name) else {
                    self.unresolved_reference(
                        pending.occurrence,
                        condition.span,
                        "ACL",
                        &condition.name,
                        &[],
                        "is not defined in this section",
                    );
                    unresolved = true;
                    continue;
                };
                let acl_targets = acls
                    .iter()
                    .map(|acl| ReferenceTarget {
                        occurrence: acl.provenance.origin,
                        span: acl.provenance.origin_span,
                    })
                    .collect::<Vec<_>>();
                conditions.push(AclReference {
                    name: condition.name.clone(),
                    definitions: acl_targets.iter().map(|target| target.occurrence).collect(),
                });
                references.push(ReferenceProvenance {
                    use_span: condition.span,
                    targets: acl_targets,
                });
            }
            if unresolved {
                continue;
            }
            state.use_backends.push(EffectiveValue::direct_references(
                UseBackend {
                    backend: BackendReference {
                        name: pending.backend_name,
                        target,
                    },
                    conditions,
                    polarity: pending.polarity,
                    condition_negated: pending.condition_negated,
                },
                pending.occurrence,
                pending.span,
                references,
            ));
            self.consume(pending.occurrence, Consumption::Reference);
        }
    }

    fn resolve_defaults_reference(
        &mut self,
        occurrence: OccurrenceId,
        span: Span,
        name: &[u8],
    ) -> Option<usize> {
        let candidates = self.defaults_by_name.get(name).cloned().unwrap_or_default();
        if candidates.len() == 1 {
            return candidates.first().copied();
        }
        let related = candidates
            .iter()
            .map(|index| self.sections[*index].section.header.span)
            .collect::<Vec<_>>();
        let reason = if candidates.is_empty() {
            "is not declared"
        } else {
            "is ambiguous"
        };
        self.unresolved_reference(occurrence, span, "defaults", name, &related, reason);
        None
    }

    fn resolve_backend_reference(
        &mut self,
        occurrence: OccurrenceId,
        span: Span,
        name: &[u8],
    ) -> Option<(SectionId, ReferenceTarget)> {
        let candidates = self.backends_by_name.get(name).cloned().unwrap_or_default();
        if let [index] = candidates.as_slice() {
            let target = &self.sections[*index];
            return Some((
                target.id,
                ReferenceTarget {
                    occurrence: OccurrenceId::SectionHeader(target.id),
                    span: target.section.header.span,
                },
            ));
        }
        let related = candidates
            .iter()
            .map(|index| self.sections[*index].section.header.span)
            .collect::<Vec<_>>();
        let reason = if candidates.is_empty() {
            "is not declared"
        } else {
            "is ambiguous"
        };
        self.unresolved_reference(occurrence, span, "backend", name, &related, reason);
        None
    }

    fn unresolved_reference(
        &mut self,
        occurrence: OccurrenceId,
        span: Span,
        reference_kind: &str,
        name: &[u8],
        related: &[Span],
        reason: &str,
    ) {
        self.block(occurrence, BlockingReason::UnresolvedReference);
        let mut diagnostic = Diagnostic::new(
            E_UNRESOLVED_REFERENCE,
            Severity::Error,
            DiagnosticStage::Resolve,
            format!(
                "HAProxy {reference_kind} reference `{}` {reason}",
                display_bytes(name)
            ),
        )
        .with_primary_span(span);
        for target in related {
            diagnostic = diagnostic.with_related_span(*target, "candidate declaration is here");
        }
        self.diagnostics.push(diagnostic);
    }

    fn reject_unsupported_section(&mut self, index: usize) {
        let meta = self.sections[index].clone();
        let occurrence = OccurrenceId::SectionHeader(meta.id);
        self.block(occurrence, BlockingReason::UnsupportedSection);
        self.diagnostics.push(
            Diagnostic::new(
                E_UNSUPPORTED_SECTION,
                Severity::Error,
                DiagnosticStage::Resolve,
                format!(
                    "HAProxy {} sections are not represented by the import IR",
                    section_name(meta.section.kind)
                ),
            )
            .with_primary_span(meta.section.header.span),
        );
        self.block_section_directives(index, BlockingReason::UnsupportedSection);
    }

    fn block_section_directives(&mut self, index: usize, reason: BlockingReason) {
        let id = self.sections[index].id;
        let count = self.sections[index].section.directives.len();
        for directive_ordinal in 0..count {
            self.block(section_directive_id(id, directive_ordinal), reason);
        }
    }

    fn block_preprocessing(&mut self, occurrence: OccurrenceId, directive: &Directive) -> bool {
        if is_conditional(directive) {
            self.block(occurrence, BlockingReason::ConditionalPreprocessing);
            self.diagnostics.push(
                Diagnostic::new(
                    E_CONDITIONAL_PREPROCESSING,
                    Severity::Error,
                    DiagnosticStage::Resolve,
                    "HAProxy conditional requires explicit preprocessing before activation",
                )
                .with_primary_span(directive.name.span),
            );
            return true;
        }
        self.block_environment(occurrence, directive)
    }

    fn block_environment(&mut self, occurrence: OccurrenceId, directive: &Directive) -> bool {
        let references = directive
            .arguments
            .iter()
            .chain(std::iter::once(&directive.name))
            .flat_map(|word| word.environment_references.iter().copied())
            .collect::<Vec<_>>();
        if references.is_empty() {
            return false;
        }
        self.block(occurrence, BlockingReason::EnvironmentPreprocessing);
        for reference in references {
            self.diagnostics.push(
                Diagnostic::new(
                    E_ENVIRONMENT_EXPANSION,
                    Severity::Error,
                    DiagnosticStage::Resolve,
                    "HAProxy environment reference requires explicit preprocessing before activation",
                )
                .with_primary_span(reference),
            );
        }
        true
    }

    fn reject_semantic_directive(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        message: &str,
    ) {
        self.block(occurrence, BlockingReason::SemanticBlocker);
        self.diagnostics.push(
            Diagnostic::new(
                E_UNSUPPORTED_FORM,
                Severity::Error,
                DiagnosticStage::Resolve,
                message,
            )
            .with_primary_span(directive.span),
        );
    }

    fn track_and_reject_semantics(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        kind: SemanticBlockerKind,
        state: &mut SectionState,
        message: &str,
    ) {
        state
            .settings
            .semantic_blockers
            .push(semantic_blocker(kind, occurrence, directive));
        self.reject_semantic_directive(occurrence, directive, message);
    }

    fn block_bind_semantics(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        state: &mut SectionState,
        message: &str,
    ) {
        self.track_and_reject_semantics(
            occurrence,
            directive,
            SemanticBlockerKind::Tls,
            state,
            message,
        );
    }

    fn conflicting_directive(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        first_span: Span,
    ) {
        self.block(occurrence, BlockingReason::ConflictingDirective);
        self.diagnostics.push(
            Diagnostic::new(
                E_CONFLICTING_DIRECTIVE,
                Severity::Error,
                DiagnosticStage::Resolve,
                format!(
                    "conflicting HAProxy `{}` directives cannot select one effective value",
                    display_bytes(&directive.name.value)
                ),
            )
            .with_primary_span(directive.span)
            .with_related_span(first_span, "first direct value is here"),
        );
    }

    fn conflicting_option(
        &mut self,
        occurrence: OccurrenceId,
        current_span: Span,
        previous_span: Span,
        name: &[u8],
    ) {
        self.block(occurrence, BlockingReason::ConflictingDirective);
        self.diagnostics.push(
            Diagnostic::new(
                E_CONFLICTING_DIRECTIVE,
                Severity::Error,
                DiagnosticStage::Resolve,
                format!(
                    "conflicting HAProxy `{}` options cannot select one effective value",
                    display_bytes(name)
                ),
            )
            .with_primary_span(current_span)
            .with_related_span(previous_span, "first option is here"),
        );
    }

    fn finish_setting(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        conflict: Option<Span>,
        settings: &mut ProxySettings,
    ) -> bool {
        let Some(first_span) = conflict else {
            return false;
        };
        self.conflicting_directive(occurrence, directive, first_span);
        settings.semantic_blockers.push(semantic_blocker(
            SemanticBlockerKind::ConflictingDirective,
            occurrence,
            directive,
        ));
        true
    }

    fn externalize_process_setting(&mut self, occurrence: OccurrenceId, directive: &Directive) {
        let decision = self.pending_decision_mut(occurrence);
        if decision.outcome.is_none() {
            decision.outcome = Some(DecisionOutcome::Externalized(Externalization::ProcessOwned));
        }
        self.diagnostics.push(
            Diagnostic::new(
                E_PROCESS_OWNED,
                Severity::Warning,
                DiagnosticStage::Resolve,
                "HAProxy process-owned behavior is externalized to the deployment",
            )
            .with_primary_span(directive.span),
        );
        self.effective
            .deployment_requirements
            .push(DeploymentRequirement {
                kind: process_requirement_kind(&directive.name.value),
                directive: display_bytes(&directive.name.value),
                value: directive
                    .arguments
                    .iter()
                    .map(|argument| display_bytes(&argument.value))
                    .collect(),
                origin: ProvenanceSpan {
                    role: ProvenanceRole::Value,
                    span: directive.span,
                },
            });
    }

    fn externalize_log_transport(&mut self, occurrence: OccurrenceId, directive: &Directive) {
        let decision = self.pending_decision_mut(occurrence);
        if decision.outcome.is_none() {
            decision.outcome = Some(DecisionOutcome::Externalized(Externalization::LogTransport));
        }
        self.diagnostics.push(
            Diagnostic::new(
                E_LOGGING_UNSUPPORTED,
                Severity::Warning,
                DiagnosticStage::Resolve,
                "HAProxy log transport is externalized to the deployment; no format equivalence is claimed",
            )
            .with_primary_span(directive.span),
        );
        self.effective
            .deployment_requirements
            .push(DeploymentRequirement {
                kind: DeploymentRequirementKind::LogTransport,
                directive: display_bytes(&directive.name.value),
                value: directive
                    .arguments
                    .iter()
                    .map(|argument| display_bytes(&argument.value))
                    .collect(),
                origin: ProvenanceSpan {
                    role: ProvenanceRole::Value,
                    span: directive.span,
                },
            });
    }

    fn externalize_activation(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        kind: ActivationRequirementKind,
        section: Option<SectionId>,
        supported: bool,
    ) {
        let decision = self.pending_decision_mut(occurrence);
        if decision.outcome.is_none() {
            decision.outcome = Some(DecisionOutcome::Externalized(Externalization::Activation));
        }
        self.diagnostics.push(
            Diagnostic::new(
                E_STATS_UNSUPPORTED,
                Severity::Warning,
                DiagnosticStage::Resolve,
                "HAProxy statistics endpoint requires explicit activation; no runtime equivalence is claimed",
            )
            .with_primary_span(directive.span),
        );
        self.effective
            .activation_requirements
            .push(ActivationRequirement {
                kind,
                directive: display_directive(directive),
                origin: ProvenanceSpan {
                    role: ProvenanceRole::Value,
                    span: directive.span,
                },
                equivalent_runtime_endpoint: false,
            });
        if let Some(section) = section {
            self.effective.activation_only_sections.insert(section);
            if supported {
                self.effective.supported_stats_sections.insert(section);
            }
        }
    }

    fn unknown_directive(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        location: &str,
    ) {
        self.block(occurrence, BlockingReason::UnknownDirective);
        self.diagnostics.push(
            Diagnostic::new(
                E_UNKNOWN_DIRECTIVE,
                Severity::Error,
                DiagnosticStage::Resolve,
                format!(
                    "unknown HAProxy directive `{}` {location}",
                    display_bytes(&directive.name.value)
                ),
            )
            .with_primary_span(directive.name.span),
        );
    }

    fn unsupported_directive_form(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
        kind: SectionKind,
    ) {
        self.unsupported_form(
            occurrence,
            directive.span,
            format!(
                "unsupported HAProxy `{}` form in a {} section",
                display_bytes(&directive.name.value),
                section_name(kind)
            ),
        );
    }

    fn unsupported_directive_form_for_occurrence(
        &mut self,
        occurrence: OccurrenceId,
        directive: &Directive,
    ) {
        let kind = self
            .section_kind(occurrence)
            .expect("directive occurrence belongs to a section");
        self.unsupported_directive_form(occurrence, directive, kind);
    }

    fn unsupported_form(&mut self, occurrence: OccurrenceId, span: Span, message: String) {
        self.block(occurrence, BlockingReason::UnsupportedForm);
        self.diagnostics.push(
            Diagnostic::new(
                E_UNSUPPORTED_FORM,
                Severity::Error,
                DiagnosticStage::Resolve,
                message,
            )
            .with_primary_span(span),
        );
    }

    fn set_setting<T: PartialEq>(
        &mut self,
        slot: &mut Option<EffectiveValue<T>>,
        value: EffectiveValue<T>,
    ) -> Option<Span> {
        set_value(slot, value, &mut self.decisions, &self.decision_indices)
    }

    fn consume(&mut self, occurrence: OccurrenceId, consumption: Consumption) {
        let decision = self.pending_decision_mut(occurrence);
        if decision.outcome.is_none() {
            decision.outcome = Some(DecisionOutcome::Consumed(consumption));
        }
    }

    fn block(&mut self, occurrence: OccurrenceId, reason: BlockingReason) {
        let decision = self.pending_decision_mut(occurrence);
        if decision.outcome.is_none() {
            decision.outcome = Some(DecisionOutcome::Blocked(reason));
        }
    }

    fn pending_decision_mut(&mut self, occurrence: OccurrenceId) -> &mut PendingDecision {
        let index = self.decision_indices[&occurrence];
        &mut self.decisions[index]
    }

    fn section(&self, id: SectionId) -> &SectionMeta {
        self.sections
            .iter()
            .find(|section| section.id == id)
            .expect("section occurrence was indexed")
    }

    fn section_kind(&self, occurrence: OccurrenceId) -> Option<SectionKind> {
        match occurrence {
            OccurrenceId::Preamble { .. } => None,
            OccurrenceId::SectionHeader(id)
            | OccurrenceId::SectionDirective { section: id, .. } => {
                Some(self.section(id).section.kind)
            }
        }
    }

    fn finish_ledger(&mut self) {
        for decision in &mut self.decisions {
            if decision.outcome.is_some() {
                continue;
            }
            decision.outcome = Some(DecisionOutcome::Blocked(
                BlockingReason::UnconsumedDirective,
            ));
            self.diagnostics.push(
                Diagnostic::new(
                    E_UNCONSUMED_DIRECTIVE,
                    Severity::Error,
                    DiagnosticStage::Resolve,
                    format!(
                        "HAProxy occurrence `{}` was not consumed by semantic resolution",
                        display_bytes(&decision.keyword)
                    ),
                )
                .with_primary_span(decision.span),
            );
        }
        self.effective.ledger = DecisionLedger {
            entries: self
                .decisions
                .drain(..)
                .map(|decision| Decision {
                    occurrence: decision.occurrence,
                    section: decision.section,
                    keyword: decision.keyword,
                    span: decision.span,
                    outcome: decision.outcome.expect("terminal decision was assigned"),
                })
                .collect(),
        };
    }
}

fn push_pending_decision(
    decisions: &mut Vec<PendingDecision>,
    indices: &mut HashMap<OccurrenceId, usize>,
    occurrence: OccurrenceId,
    section: Option<SectionId>,
    directive: &Directive,
) {
    let index = decisions.len();
    decisions.push(PendingDecision {
        occurrence,
        section,
        keyword: directive.name.value.clone(),
        span: directive.span,
        outcome: None,
    });
    indices.insert(occurrence, index);
}

fn semantic_blocker(
    kind: SemanticBlockerKind,
    occurrence: OccurrenceId,
    directive: &Directive,
) -> EffectiveValue<SemanticBlocker> {
    EffectiveValue::direct(
        SemanticBlocker {
            kind,
            keyword: directive.name.value.clone(),
            arguments: directive
                .arguments
                .iter()
                .map(|argument| argument.value.clone())
                .collect(),
        },
        occurrence,
        directive.span,
    )
}

fn set_value<T: PartialEq>(
    slot: &mut Option<EffectiveValue<T>>,
    value: EffectiveValue<T>,
    decisions: &mut [PendingDecision],
    indices: &HashMap<OccurrenceId, usize>,
) -> Option<Span> {
    let conflict = slot
        .as_ref()
        .filter(|previous| previous.provenance.is_direct() && previous.value != value.value)
        .map(|previous| previous.provenance.origin_span);
    if let Some(previous) = slot
        .as_ref()
        .filter(|previous| previous.provenance.is_direct())
    {
        let index = indices[&previous.provenance.origin];
        if matches!(decisions[index].outcome, Some(DecisionOutcome::Consumed(_))) {
            decisions[index].outcome = Some(DecisionOutcome::Superseded {
                by: value.provenance.origin,
            });
        }
    }
    *slot = Some(value);
    conflict
}

fn set_idempotent_value<T: PartialEq>(
    slot: &mut Option<EffectiveValue<T>>,
    value: EffectiveValue<T>,
    decisions: &mut [PendingDecision],
    indices: &HashMap<OccurrenceId, usize>,
) -> Result<(), Span> {
    if slot
        .as_ref()
        .is_some_and(|current| current.provenance.is_direct() && current.value == value.value)
    {
        return Ok(());
    }
    set_value(slot, value, decisions, indices).map_or(Ok(()), Err)
}

fn parse_header(kind: SectionKind, directive: &Directive) -> Result<ParsedHeader, &'static str> {
    let arguments = directive.arguments.as_slice();
    match kind {
        SectionKind::Global if arguments.is_empty() => Ok(ParsedHeader {
            name: None,
            from: None,
        }),
        SectionKind::Global => Err("`global` takes no arguments"),
        SectionKind::Defaults => match arguments {
            [] => Ok(ParsedHeader {
                name: None,
                from: None,
            }),
            [name] if name.value != b"from" => Ok(ParsedHeader {
                name: Some((name.value.clone(), name.span)),
                from: None,
            }),
            [from, target] if from.value == b"from" => Ok(ParsedHeader {
                name: None,
                from: Some((target.value.clone(), target.span)),
            }),
            [name, from, target] if name.value != b"from" && from.value == b"from" => {
                Ok(ParsedHeader {
                    name: Some((name.value.clone(), name.span)),
                    from: Some((target.value.clone(), target.span)),
                })
            }
            _ => Err("expected `defaults [name] [from defaults_name]`"),
        },
        SectionKind::Frontend | SectionKind::Backend | SectionKind::Listen => match arguments {
            [name] if name.value != b"from" => Ok(ParsedHeader {
                name: Some((name.value.clone(), name.span)),
                from: None,
            }),
            [name, from, target] if name.value != b"from" && from.value == b"from" => {
                Ok(ParsedHeader {
                    name: Some((name.value.clone(), name.span)),
                    from: Some((target.value.clone(), target.span)),
                })
            }
            _ => Err("expected a name followed by optional `from defaults_name`"),
        },
        _ => Err("section is not supported"),
    }
}

fn effective_section(meta: &SectionMeta, header: &ParsedHeader) -> EffectiveSection {
    EffectiveSection {
        id: meta.id,
        declaration: OccurrenceId::SectionHeader(meta.id),
        name: header.name.as_ref().map(|(name, _)| name.clone()),
        span: meta.section.header.span,
    }
}

fn defaults_source(
    meta: &SectionMeta,
    target: SectionId,
    target_occurrence: OccurrenceId,
    target_span: Span,
    selection: DefaultsSelection,
    reference_span: Span,
) -> DefaultsSource {
    DefaultsSource {
        section: target,
        selection,
        provenance: Provenance::direct(
            OccurrenceId::SectionHeader(meta.id),
            meta.section.header.span,
        )
        .with_reference(
            reference_span,
            vec![ReferenceTarget {
                occurrence: target_occurrence,
                span: target_span,
            }],
        ),
    }
}

fn section_directive_id(section: SectionId, directive_ordinal: usize) -> OccurrenceId {
    OccurrenceId::SectionDirective {
        section,
        directive_ordinal,
    }
}

fn exactly_one_argument(directive: &Directive) -> Option<&super::Word> {
    let [argument] = directive.arguments.as_slice() else {
        return None;
    };
    Some(argument)
}

fn parse_one_u32(directive: &Directive) -> Option<u32> {
    parse_u32(&exactly_one_argument(directive)?.value)
}

fn parse_one_u64(directive: &Directive) -> Option<u64> {
    parse_u64(&exactly_one_argument(directive)?.value)
}

fn parse_u16(value: &[u8]) -> Option<u16> {
    std::str::from_utf8(value).ok()?.parse().ok()
}

fn parse_retry_on(arguments: &[super::Word]) -> Result<RetryOn, String> {
    if arguments.is_empty() {
        return Err("requires at least one error form".into());
    }
    if arguments.len() == 1 && arguments[0].value == b"none" {
        return Ok(RetryOn::None);
    }
    if arguments.iter().any(|argument| argument.value == b"none") {
        return Err("none cannot be combined with another error form".into());
    }
    if arguments
        .iter()
        .any(|argument| matches!(argument.value.as_slice(), b"all" | b"all-retryable-errors"))
    {
        if arguments.len() != 1 {
            return Err(
                "all and all-retryable-errors cannot be combined with another error form".into(),
            );
        }
        return Ok(RetryOn::Rules {
            triggers: vec![
                RetryOnTrigger::ConnFailure,
                RetryOnTrigger::EmptyResponse,
                RetryOnTrigger::ResponseTimeout,
                RetryOnTrigger::JunkResponse,
            ],
            response_statuses: (500..=599).collect(),
        });
    }

    let mut seen = HashSet::new();
    let mut triggers = Vec::new();
    let mut response_statuses = Vec::new();
    for argument in arguments {
        if !seen.insert(argument.value.clone()) {
            return Err(format!(
                "duplicate error form `{}`",
                display_bytes(&argument.value)
            ));
        }
        let trigger = match argument.value.as_slice() {
            b"conn-failure" | b"conn-refused" => Some(RetryOnTrigger::ConnFailure),
            b"empty-response" => Some(RetryOnTrigger::EmptyResponse),
            b"response-timeout" => Some(RetryOnTrigger::ResponseTimeout),
            b"junk-response" => Some(RetryOnTrigger::JunkResponse),
            b"all" | b"all-retryable-errors" => unreachable!("all forms returned above"),
            b"0rtt-rejected" => {
                return Err("0rtt-rejected is outside the supported retry trigger subset".into());
            }
            _ => None,
        };
        if let Some(trigger) = trigger {
            triggers.push(trigger);
            continue;
        }

        let status = parse_u16(&argument.value)
            .filter(|_| argument.value.len() == 3 && argument.value.iter().all(u8::is_ascii_digit))
            .ok_or_else(|| {
                format!(
                    "unsupported error form `{}`",
                    display_bytes(&argument.value)
                )
            })?;
        if !(500..=599).contains(&status) {
            return Err(format!(
                "status `{}` is not a supported 5xx retry status",
                display_bytes(&argument.value)
            ));
        }
        if response_statuses.contains(&status) {
            return Err(format!(
                "duplicate error form `{}`",
                display_bytes(&argument.value)
            ));
        }
        response_statuses.push(status);
    }
    response_statuses.sort_unstable();
    Ok(RetryOn::Rules {
        triggers,
        response_statuses,
    })
}

fn parse_u32(value: &[u8]) -> Option<u32> {
    std::str::from_utf8(value).ok()?.parse().ok()
}

fn parse_u64(value: &[u8]) -> Option<u64> {
    std::str::from_utf8(value).ok()?.parse().ok()
}

fn parse_i32(value: &[u8]) -> Option<i32> {
    std::str::from_utf8(value).ok()?.parse().ok()
}

fn parse_duration(value: &[u8]) -> Option<Duration> {
    let number_len = value
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if number_len == 0 {
        return None;
    }
    let amount = parse_u64(&value[..number_len])?;
    match &value[number_len..] {
        b"us" => Some(Duration::from_micros(amount)),
        b"" | b"ms" => Some(Duration::from_millis(amount)),
        b"s" => Some(Duration::from_secs(amount)),
        b"m" => Some(Duration::from_secs(amount.checked_mul(60)?)),
        b"h" => Some(Duration::from_secs(amount.checked_mul(60 * 60)?)),
        b"d" => Some(Duration::from_secs(amount.checked_mul(24 * 60 * 60)?)),
        _ => None,
    }
}

fn parse_bind_address(value: &[u8]) -> Option<BindAddress> {
    let unix_path = value.strip_prefix(b"unix@").unwrap_or(value);
    if unix_path.starts_with(b"/") {
        return Some(BindAddress::Unix {
            path: unix_path.to_vec(),
        });
    }
    let (host, port) = parse_host_port(value)?;
    Some(BindAddress::Tcp { host, port })
}

enum BindParseError {
    Malformed,
    Semantic(String),
    Conflict {
        name: Vec<u8>,
        current_span: Span,
        previous_span: Span,
    },
}

#[derive(Default)]
struct BindOptions<'a> {
    ssl: Option<Span>,
    certificate: Option<&'a super::Word>,
    alpn: Option<(Vec<TlsAlpn>, Span)>,
    minimum_version: Option<(TlsMinimumVersion, Span)>,
    maxconn: Option<(u64, Span)>,
    mode: Option<(u16, Span)>,
}

fn parse_bind(
    directive: &Directive,
    occurrence: OccurrenceId,
) -> Result<Vec<EffectiveBind>, BindParseError> {
    let (address_word, option_words) = directive
        .arguments
        .split_first()
        .ok_or(BindParseError::Malformed)?;
    let addresses = address_word
        .value
        .split(|byte| *byte == b',')
        .map(parse_bind_address)
        .collect::<Option<Vec<_>>>()
        .ok_or(BindParseError::Malformed)?;
    let options = parse_bind_options(option_words)?;
    let tls = finish_bind_tls(&options, occurrence)?;
    if options.mode.is_some()
        && addresses
            .iter()
            .any(|address| !matches!(address, BindAddress::Unix { .. }))
    {
        return Err(BindParseError::Semantic(
            "HAProxy bind mode applies only to Unix sockets".into(),
        ));
    }
    let mode = options
        .mode
        .map(|(value, span)| EffectiveValue::direct(value, occurrence, span));
    let maxconn = options
        .maxconn
        .map(|(value, span)| EffectiveValue::direct(value, occurrence, span));
    Ok(addresses
        .into_iter()
        .map(|address| EffectiveBind {
            address: EffectiveValue::direct(address, occurrence, address_word.span),
            mode: mode.clone(),
            maxconn: maxconn.clone(),
            tls: tls.clone(),
        })
        .collect())
}

fn parse_bind_options(options: &[super::Word]) -> Result<BindOptions<'_>, BindParseError> {
    let mut parsed = BindOptions::default();
    let mut index = 0;
    while index < options.len() {
        let option = &options[index];
        match option.value.as_slice() {
            b"ssl" if parsed.ssl.is_none() => {
                parsed.ssl = Some(option.span);
                index += 1;
            }
            b"crt" if parsed.certificate.is_none() => {
                parsed.certificate = Some(options.get(index + 1).ok_or_else(|| {
                    BindParseError::Semantic(
                        "HAProxy bind certificate selection is incomplete".into(),
                    )
                })?);
                index += 2;
            }
            b"alpn" if parsed.alpn.is_none() => {
                let protocols = options.get(index + 1).ok_or_else(|| {
                    BindParseError::Semantic("HAProxy bind ALPN policy is incomplete".into())
                })?;
                let alpn = parse_tls_alpn(&protocols.value).ok_or_else(|| {
                    BindParseError::Semantic(
                        "HAProxy bind ALPN policy is not exactly representable".into(),
                    )
                })?;
                parsed.alpn = Some((alpn, protocols.span));
                index += 2;
            }
            b"ssl-min-ver" if parsed.minimum_version.is_none() => {
                let version = options.get(index + 1).ok_or_else(|| {
                    BindParseError::Semantic(
                        "HAProxy bind TLS minimum version is incomplete".into(),
                    )
                })?;
                let minimum_version = match version.value.as_slice() {
                    b"TLSv1.2" => TlsMinimumVersion::Tls12,
                    b"TLSv1.3" => TlsMinimumVersion::Tls13,
                    _ => {
                        return Err(BindParseError::Semantic(
                            "HAProxy bind TLS minimum version is not represented canonically"
                                .into(),
                        ));
                    }
                };
                parsed.minimum_version = Some((minimum_version, version.span));
                index += 2;
            }
            b"maxconn" if parsed.maxconn.is_none() => {
                let value = options.get(index + 1).ok_or_else(|| {
                    BindParseError::Semantic(
                        "HAProxy bind maxconn requires an unsigned integer".into(),
                    )
                })?;
                let maxconn = parse_u64(&value.value).ok_or_else(|| {
                    BindParseError::Semantic(
                        "HAProxy bind maxconn requires an unsigned integer".into(),
                    )
                })?;
                parsed.maxconn = Some((maxconn, value.span));
                index += 2;
            }
            b"mode" if parsed.mode.is_none() => {
                parsed.mode = Some(parse_bind_mode(options.get(index + 1))?);
                index += 2;
            }
            b"crt" | b"crt-list" => {
                return Err(BindParseError::Semantic(
                    "HAProxy bind certificate selection uses crt-list or multiple crt parameters"
                        .into(),
                ));
            }
            b"ssl" | b"alpn" | b"ssl-min-ver" | b"maxconn" | b"mode" => {
                let previous_span = match option.value.as_slice() {
                    b"ssl" => parsed.ssl,
                    b"alpn" => parsed.alpn.as_ref().map(|(_, span)| *span),
                    b"ssl-min-ver" => parsed.minimum_version.map(|(_, span)| span),
                    b"maxconn" => parsed.maxconn.map(|(_, span)| span),
                    _ => parsed.mode.map(|(_, span)| span),
                }
                .expect("duplicate option has a first span");
                return Err(BindParseError::Conflict {
                    name: option.value.clone(),
                    current_span: option.span,
                    previous_span,
                });
            }
            _ => {
                return Err(BindParseError::Semantic(
                    "HAProxy bind option is not represented by the import IR".into(),
                ));
            }
        }
    }
    Ok(parsed)
}

fn parse_bind_mode(value: Option<&super::Word>) -> Result<(u16, Span), BindParseError> {
    let value = value.ok_or_else(|| {
        BindParseError::Semantic("HAProxy bind mode requires octal permission bits".into())
    })?;
    let mode = std::str::from_utf8(&value.value)
        .ok()
        .and_then(|value| u16::from_str_radix(value, 8).ok())
        .filter(|mode| (1..=0o777).contains(mode))
        .ok_or_else(|| {
            BindParseError::Semantic(
                "HAProxy bind mode requires octal permission bits from 001 through 777".into(),
            )
        })?;
    Ok((mode, value.span))
}

fn finish_bind_tls(
    options: &BindOptions<'_>,
    occurrence: OccurrenceId,
) -> Result<Option<EffectiveValue<BindTls>>, BindParseError> {
    match (options.ssl, options.certificate) {
        (None, None) if options.alpn.is_none() && options.minimum_version.is_none() => Ok(None),
        (Some(_), Some(certificate)) => {
            let (alpn, _) = options.alpn.clone().ok_or_else(|| {
                BindParseError::Semantic(
                    "HAProxy TLS bind requires an explicit canonical ALPN policy".into(),
                )
            })?;
            let minimum_version = options
                .minimum_version
                .map_or(TlsMinimumVersion::Tls12, |(version, _)| version);
            let tls = load_bind_tls(&certificate.value, alpn, minimum_version)
                .map_err(BindParseError::Semantic)?;
            Ok(Some(EffectiveValue::direct(
                tls,
                occurrence,
                certificate.span,
            )))
        }
        _ => Err(BindParseError::Semantic(
            "HAProxy TLS bind certificate selection is incomplete".into(),
        )),
    }
}

fn parse_tls_alpn(value: &[u8]) -> Option<Vec<TlsAlpn>> {
    let protocols = value
        .split(|byte| *byte == b',')
        .map(|protocol| match protocol {
            b"h2" => Some(TlsAlpn::H2),
            b"http/1.1" => Some(TlsAlpn::Http11),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?;
    matches!(
        protocols.as_slice(),
        [TlsAlpn::Http11 | TlsAlpn::H2] | [TlsAlpn::H2, TlsAlpn::Http11]
    )
    .then_some(protocols)
}

fn load_bind_tls(
    raw_path: &[u8],
    alpn: Vec<TlsAlpn>,
    minimum_version: TlsMinimumVersion,
) -> Result<BindTls, String> {
    let path = std::str::from_utf8(raw_path)
        .map(PathBuf::from)
        .map_err(|_| "HAProxy crt path is not UTF-8".to_owned())?;
    if !path.is_absolute() {
        return Err(
            "HAProxy crt path must be absolute when no representable crt-base is available".into(),
        );
    }
    let items = read_pem_items(&path, "crt PEM")?;
    let (dns_names, leaf_certificate, embedded_private_keys) = certificate_metadata(&path, &items)?;
    if embedded_private_keys != 0 {
        return Err(format!(
            "HAProxy combined certificate/private-key bundle `{}` cannot be preserved by separate canonical file references",
            path.display()
        ));
    }

    let mut private_key_name = path.as_os_str().to_owned();
    private_key_name.push(".key");
    let private_key_path = PathBuf::from(private_key_name);
    validate_sidecar_key(&private_key_path, &leaf_certificate)?;

    Ok(BindTls {
        certificate_chain_path: path,
        private_key_path,
        dns_names,
        alpn,
        minimum_version,
    })
}

fn read_pem_items(path: &std::path::Path, kind: &str) -> Result<Vec<Item>, String> {
    let bytes = read_stable_pem(path, kind)?;
    let mut reader = BufReader::new(bytes.as_slice());
    let mut items = Vec::new();
    while let Some(item) = read_one(&mut reader)
        .map_err(|error| format!("cannot parse HAProxy {kind} `{}`: {error}", path.display()))?
    {
        items.push(item);
    }
    Ok(items)
}

fn certificate_metadata(
    path: &std::path::Path,
    items: &[Item],
) -> Result<(Vec<String>, Vec<u8>, usize), String> {
    let mut dns_names = Vec::new();
    let mut leaf_certificate = None;
    let mut certificate_count = 0usize;
    let mut private_key_count = 0usize;
    for item in items {
        match item {
            Item::X509Certificate(certificate) => {
                if certificate_count == 0 {
                    leaf_certificate = Some(certificate.as_ref().to_vec());
                }
                collect_certificate_metadata(
                    path,
                    certificate.as_ref(),
                    certificate_count,
                    &mut dns_names,
                )?;
                certificate_count += 1;
            }
            Item::Pkcs1Key(_) | Item::Pkcs8Key(_) | Item::Sec1Key(_) => private_key_count += 1,
            _ => {
                return Err(format!(
                    "HAProxy crt PEM `{}` contains an unsupported PEM item",
                    path.display()
                ));
            }
        }
    }
    if certificate_count == 0 {
        return Err(format!(
            "HAProxy crt PEM `{}` contains no certificates",
            path.display()
        ));
    }
    if certificate_count > MAX_CERTIFICATES_IN_CHAIN {
        return Err(format!(
            "HAProxy crt PEM `{}` exceeds {MAX_CERTIFICATES_IN_CHAIN} certificates",
            path.display()
        ));
    }
    validate_dns_identities(path, &dns_names)?;
    Ok((
        dns_names,
        leaf_certificate.expect("nonempty chain has a leaf certificate"),
        private_key_count,
    ))
}

fn collect_certificate_metadata(
    path: &std::path::Path,
    certificate: &[u8],
    index: usize,
    dns_names: &mut Vec<String>,
) -> Result<(), String> {
    let (remainder, parsed) = parse_x509_certificate(certificate).map_err(|_| {
        format!(
            "HAProxy crt PEM `{}` contains an invalid X.509 certificate",
            path.display()
        )
    })?;
    if !remainder.is_empty() {
        return Err(format!(
            "HAProxy crt PEM `{}` contains trailing certificate DER data",
            path.display()
        ));
    }
    let is_ca = parsed
        .basic_constraints()
        .map_err(|_| {
            format!(
                "HAProxy crt PEM `{}` has invalid basic constraints",
                path.display()
            )
        })?
        .is_some_and(|constraints| constraints.value.ca);
    if index != 0 && !is_ca {
        return Err(format!(
            "HAProxy crt PEM `{}` contains multiple end-entity certificates; multi-cert selection is unsupported",
            path.display()
        ));
    }
    if index == 0 {
        if let Some(names) = parsed.subject_alternative_name().map_err(|_| {
            format!(
                "HAProxy crt PEM `{}` has an invalid subject alternative name extension",
                path.display()
            )
        })? {
            for name in &names.value.general_names {
                let GeneralName::DNSName(name) = name else {
                    continue;
                };
                let canonical = canonical_certificate_dns_name(name).ok_or_else(|| {
                    format!(
                        "HAProxy crt PEM `{}` contains an unsupported DNS subject alternative name",
                        path.display()
                    )
                })?;
                dns_names.push(canonical);
            }
        }
    }
    Ok(())
}

fn validate_dns_identities(path: &std::path::Path, dns_names: &[String]) -> Result<(), String> {
    if dns_names.is_empty() {
        return Err(format!(
            "HAProxy crt PEM `{}` has no DNS subject alternative names",
            path.display()
        ));
    }
    if dns_names.len() > MAX_CERTIFICATE_DNS_NAMES {
        return Err(format!(
            "HAProxy crt PEM `{}` exceeds {MAX_CERTIFICATE_DNS_NAMES} DNS subject alternative names",
            path.display()
        ));
    }
    let mut unique = std::collections::HashSet::with_capacity(dns_names.len());
    if dns_names.iter().all(|name| unique.insert(name)) {
        Ok(())
    } else {
        Err(format!(
            "HAProxy crt PEM `{}` repeats a DNS subject alternative name",
            path.display()
        ))
    }
}

fn validate_sidecar_key(path: &std::path::Path, leaf_certificate: &[u8]) -> Result<(), String> {
    let bytes = Zeroizing::new(read_stable_pem(path, "crt sidecar key")?);
    let mut reader = BufReader::new(bytes.as_slice());
    let mut items = Vec::new();
    while let Some(item) = read_one(&mut reader).map_err(|error| {
        format!(
            "cannot parse HAProxy crt sidecar key `{}`: {error}",
            path.display()
        )
    })? {
        items.push(item);
    }
    let key_count = items
        .iter()
        .filter(|item| {
            matches!(
                item,
                Item::Pkcs1Key(_) | Item::Pkcs8Key(_) | Item::Sec1Key(_)
            )
        })
        .count();
    if key_count != items.len() {
        return Err(format!(
            "HAProxy crt sidecar key `{}` contains a non-key PEM item",
            path.display()
        ));
    }
    if key_count != 1 {
        return Err(format!(
            "HAProxy crt sidecar key `{}` must contain exactly one private key",
            path.display()
        ));
    }
    let private_key = PKey::private_key_from_pem(&bytes).map_err(|_| {
        format!(
            "HAProxy crt sidecar key `{}` is not a supported private key",
            path.display()
        )
    })?;
    let minimum_bits = match private_key.id() {
        Id::RSA | Id::RSA_PSS => 2_048,
        Id::EC => 256,
        _ => {
            return Err(format!(
                "HAProxy crt sidecar key `{}` uses an unsupported algorithm",
                path.display()
            ));
        }
    };
    if private_key.bits() < minimum_bits {
        return Err(format!(
            "HAProxy crt sidecar key `{}` is below the minimum key strength",
            path.display()
        ));
    }
    let certificate = X509::from_der(leaf_certificate).map_err(|_| {
        format!(
            "HAProxy crt PEM `{}` has an invalid leaf certificate",
            path.display()
        )
    })?;
    let public_key = certificate.public_key().map_err(|_| {
        format!(
            "HAProxy crt PEM for `{}` has no supported public key",
            path.display()
        )
    })?;
    if !public_key.public_eq(&private_key) {
        return Err(format!(
            "HAProxy crt sidecar key `{}` does not match the leaf certificate",
            path.display()
        ));
    }
    Ok(())
}

fn read_stable_pem(path: &std::path::Path, kind: &str) -> Result<Vec<u8>, String> {
    let mut file = File::open(path)
        .map_err(|error| format!("cannot open HAProxy {kind} `{}`: {error}", path.display()))?;
    let before = file.metadata().map_err(|error| {
        format!(
            "cannot inspect HAProxy {kind} `{}`: {error}",
            path.display()
        )
    })?;
    if !before.is_file() {
        return Err(format!(
            "HAProxy {kind} `{}` is not a regular file",
            path.display()
        ));
    }
    if before.len() > u64::try_from(MAX_CERTIFICATE_CHAIN_BYTES).unwrap_or(u64::MAX) {
        return Err(pem_size_error(path, kind));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(
            u64::try_from(MAX_CERTIFICATE_CHAIN_BYTES)
                .unwrap_or(u64::MAX)
                .saturating_add(1),
        )
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read HAProxy {kind} `{}`: {error}", path.display()))?;
    let after = file.metadata().map_err(|error| {
        format!(
            "cannot re-inspect HAProxy {kind} `{}`: {error}",
            path.display()
        )
    })?;
    if PemFingerprint::new(&before) != PemFingerprint::new(&after) {
        return Err(format!(
            "HAProxy {kind} `{}` changed while metadata was read",
            path.display()
        ));
    }
    if bytes.len() > MAX_CERTIFICATE_CHAIN_BYTES {
        return Err(pem_size_error(path, kind));
    }
    Ok(bytes)
}

fn pem_size_error(path: &std::path::Path, kind: &str) -> String {
    format!(
        "HAProxy {kind} `{}` exceeds {MAX_CERTIFICATE_CHAIN_BYTES} bytes",
        path.display()
    )
}

#[derive(Eq, PartialEq)]
struct PemFingerprint {
    length: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed_seconds: i64,
    #[cfg(unix)]
    changed_nanoseconds: i64,
}

impl PemFingerprint {
    fn new(metadata: &Metadata) -> Self {
        Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            changed_seconds: metadata.ctime(),
            #[cfg(unix)]
            changed_nanoseconds: metadata.ctime_nsec(),
        }
    }
}

fn canonical_certificate_dns_name(name: &str) -> Option<String> {
    let name = name.to_ascii_lowercase();
    if !name.is_ascii() || name.is_empty() || name.len() > 253 || name.ends_with('.') {
        return None;
    }
    let exact_name = if let Some(exact_name) = name.strip_prefix("*.") {
        exact_name
    } else {
        if name.contains('*') {
            return None;
        }
        name.as_str()
    };
    (!exact_name.is_empty()
        && exact_name.parse::<IpAddr>().is_err()
        && exact_name.split('.').all(is_valid_dns_label))
    .then_some(name)
}

fn is_valid_dns_label(label: &str) -> bool {
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
}

fn parse_host_port(value: &[u8]) -> Option<(Vec<u8>, u16)> {
    if value.starts_with(b"[") {
        let closing = value.iter().position(|byte| *byte == b']')?;
        if value.get(closing + 1) != Some(&b':') {
            return None;
        }
        let host = value.get(1..closing)?.to_vec();
        let port = parse_u16(value.get(closing + 2..)?)?;
        return (port != 0).then_some((host, port));
    }
    let colon = value.iter().rposition(|byte| *byte == b':')?;
    let host = value[..colon].to_vec();
    let port = parse_u16(value.get(colon + 1..)?)?;
    (port != 0).then_some((host, port))
}

fn is_supported_section(kind: SectionKind) -> bool {
    matches!(
        kind,
        SectionKind::Global
            | SectionKind::Defaults
            | SectionKind::Frontend
            | SectionKind::Backend
            | SectionKind::Listen
    )
}

fn supports_bind(kind: SectionKind) -> bool {
    matches!(kind, SectionKind::Frontend | SectionKind::Listen)
}

const fn supports_stats_page(kind: SectionKind) -> bool {
    matches!(kind, SectionKind::Frontend | SectionKind::Listen)
}

fn supports_default_backend(kind: SectionKind) -> bool {
    matches!(
        kind,
        SectionKind::Defaults | SectionKind::Frontend | SectionKind::Listen
    )
}

fn supports_balance(kind: SectionKind) -> bool {
    matches!(
        kind,
        SectionKind::Defaults | SectionKind::Backend | SectionKind::Listen
    )
}

fn supports_server(kind: SectionKind) -> bool {
    matches!(kind, SectionKind::Backend | SectionKind::Listen)
}

fn supports_backend_policy(kind: SectionKind) -> bool {
    matches!(
        kind,
        SectionKind::Defaults | SectionKind::Backend | SectionKind::Listen
    )
}

fn supports_maxconn(kind: SectionKind) -> bool {
    matches!(
        kind,
        SectionKind::Defaults | SectionKind::Frontend | SectionKind::Listen
    )
}

fn supports_use_backend(kind: SectionKind) -> bool {
    matches!(kind, SectionKind::Frontend | SectionKind::Listen)
}

fn supports_http_rules(kind: SectionKind) -> bool {
    matches!(
        kind,
        SectionKind::Defaults | SectionKind::Frontend | SectionKind::Backend | SectionKind::Listen
    )
}

fn is_known_resolver_directive(name: &[u8]) -> bool {
    matches!(
        name,
        b"bind"
            | b"default_backend"
            | b"balance"
            | b"server"
            | b"retries"
            | b"maxconn"
            | b"use_backend"
            | b"http-check"
            | b"http-request"
            | b"http-response"
    )
}

fn is_global_security_directive(name: &[u8]) -> bool {
    matches!(
        name,
        b"ca-base"
            | b"crt-base"
            | b"hard-stop-after"
            | b"ssl-default-bind-ciphers"
            | b"ssl-default-bind-ciphersuites"
            | b"ssl-default-bind-curves"
            | b"ssl-default-bind-options"
            | b"ssl-default-server-ciphers"
            | b"ssl-default-server-ciphersuites"
            | b"ssl-default-server-curves"
            | b"ssl-default-server-options"
            | b"ssl-dh-param-file"
            | b"tune.ssl.cachesize"
            | b"tune.ssl.default-dh-param"
            | b"tune.ssl.lifetime"
    )
}

fn is_proxy_default_directive(name: &[u8]) -> bool {
    matches!(
        name,
        b"dispatch"
            | b"fullconn"
            | b"hash-type"
            | b"http-reuse"
            | b"http-send-name-header"
            | b"load-server-state-from-file"
            | b"server-state-file"
            | b"server-template"
            | b"source"
            | b"transparent"
    )
}

fn is_conditional(directive: &Directive) -> bool {
    matches!(
        directive.name.value.as_slice(),
        b".if" | b".elif" | b".else" | b".endif"
    )
}

fn is_process_owned(name: &[u8]) -> bool {
    matches!(
        name,
        b"chroot"
            | b"cpu-map"
            | b"daemon"
            | b"group"
            | b"master-worker"
            | b"nbproc"
            | b"nbthread"
            | b"pidfile"
            | b"setgid"
            | b"setuid"
            | b"user"
    )
}

fn process_requirement_kind(name: &[u8]) -> DeploymentRequirementKind {
    match name {
        b"user" | b"setuid" => DeploymentRequirementKind::ProcessUser,
        b"group" | b"setgid" => DeploymentRequirementKind::ProcessGroup,
        b"chroot" => DeploymentRequirementKind::Chroot,
        b"daemon" | b"master-worker" | b"pidfile" => DeploymentRequirementKind::Daemonization,
        _ => DeploymentRequirementKind::WorkerModel,
    }
}

fn is_logging_directive(directive: &Directive) -> bool {
    is_logging_directive_name(&directive.name.value)
        || (directive.name.value == b"option"
            && directive
                .arguments
                .first()
                .is_some_and(|option| is_logging_option(&option.value)))
}

fn is_logging_directive_name(name: &[u8]) -> bool {
    matches!(
        name,
        b"log" | b"log-format" | b"error-log-format" | b"unique-id-format" | b"unique-id-header"
    )
}

fn is_logging_option(name: &[u8]) -> bool {
    matches!(name, b"dontlognull" | b"httplog" | b"logasap" | b"tcplog")
}

fn section_name(kind: SectionKind) -> &'static str {
    match kind {
        SectionKind::Global => "global",
        SectionKind::Defaults => "defaults",
        SectionKind::Frontend => "frontend",
        SectionKind::Backend => "backend",
        SectionKind::Listen => "listen",
        SectionKind::Userlist => "userlist",
        SectionKind::Peers => "peers",
        SectionKind::Mailers => "mailers",
        SectionKind::NamespaceList => "namespace_list",
        SectionKind::Traces => "traces",
        SectionKind::Resolvers => "resolvers",
        SectionKind::Cache => "cache",
        SectionKind::FcgiApp => "fcgi-app",
        SectionKind::Ring => "ring",
        SectionKind::LogForward => "log-forward",
        SectionKind::LogProfile => "log-profile",
        SectionKind::HttpErrors => "http-errors",
        SectionKind::CrtStore => "crt-store",
        SectionKind::Acme => "acme",
        SectionKind::Healthcheck => "healthcheck",
        SectionKind::Program => "program",
    }
}

fn display_bytes(value: &[u8]) -> String {
    String::from_utf8_lossy(value).into_owned()
}

fn display_directive(directive: &Directive) -> String {
    std::iter::once(directive.name.value.as_slice())
        .chain(
            directive
                .arguments
                .iter()
                .map(|argument| argument.value.as_slice()),
        )
        .map(display_bytes)
        .collect::<Vec<_>>()
        .join(" ")
}

fn exact_prometheus_exporter(directive: &Directive) -> bool {
    directive
        .arguments
        .iter()
        .map(|argument| argument.value.as_slice())
        .eq([
            b"use-service".as_slice(),
            b"prometheus-exporter".as_slice(),
            b"if".as_slice(),
            b"{".as_slice(),
            b"path".as_slice(),
            b"/metrics".as_slice(),
            b"}".as_slice(),
        ])
}
