#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Provenance {
    pub span: Span,
    pub include_stack: Vec<Span>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ImportReport {
    pub source_graph: SourceGraph,
    /// Convenience copy for consumers that only need source bytes and paths.
    pub sources: Vec<SourceFile>,
    pub declarations: Vec<DeclarationDecision>,
    pub imports: Vec<VmodImport>,
    pub vmod_objects: Vec<VmodObject>,
    pub acls: Vec<Acl>,
    pub probes: Vec<Probe>,
    pub backends: Vec<Backend>,
    pub directors: Vec<Director>,
    pub modern_directors: Vec<ModernDirector>,
    pub subroutines: Vec<Subroutine>,
    pub compositions: Vec<SubroutineComposition>,
    pub call_graph: CallGraph,
    /// Pre-order statement ledger. Every retained AST statement contributes exactly one entry.
    pub statements: Vec<StatementDecision>,
    pub invocation: InvocationFacts,
    pub diagnostics: Vec<Diagnostic>,
    pub candidate: CanonicalCandidate<Provenance>,
    pub lowering: LoweringStatus,
}

impl ImportReport {
    #[must_use]
    pub fn has_errors(&self) -> bool {
        self.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity() == Severity::Error)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoweringStatus {
    Lowered,
    Blocked(LoweringBlocker),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoweringBlocker {
    NoCanonicalGraph,
    InvalidSource,
    UnsupportedBehavior,
    UnsupportedSubroutine,
    SemanticMismatch,
    Invocation,
    Validation,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeclarationDecision {
    pub sequence: usize,
    pub provenance: Provenance,
    pub version: VersionContext,
    pub classification: DeclarationClassification,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionContext {
    pub effective: Option<VclVersion>,
    pub origin: VersionOrigin,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VersionOrigin {
    Declared,
    SourceDeclared,
    IncludeInherited,
    Missing,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DeclarationClassification {
    Version(VclVersion),
    Include {
        path: Vec<u8>,
        glob: bool,
        resolved: bool,
    },
    Import {
        module: Vec<u8>,
        alias: Vec<u8>,
        from: Option<Vec<u8>>,
        index: usize,
    },
    Acl {
        name: Vec<u8>,
        index: usize,
    },
    Probe {
        name: Vec<u8>,
        index: usize,
    },
    Backend {
        name: Vec<u8>,
        index: usize,
    },
    Director {
        name: Vec<u8>,
        index: usize,
    },
    Subroutine {
        name: Vec<u8>,
        index: usize,
    },
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmodImport {
    pub module: Vec<u8>,
    pub alias: Vec<u8>,
    pub from: Option<Vec<u8>>,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmodObject {
    pub name: Vec<u8>,
    pub module: Vec<u8>,
    pub constructor: Expression,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Acl {
    pub name: Vec<u8>,
    pub entries: Vec<AclEntry>,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AclEntry {
    pub negated: bool,
    pub optional: bool,
    pub value: Vec<u8>,
    pub mask: Option<Vec<u8>>,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Probe {
    pub name: Vec<u8>,
    pub properties: Vec<ProbeProperty>,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProbeProperty {
    Url(Expression),
    Request(Expression),
    ExpectedResponse(Expression),
    Timeout(Expression),
    Interval(Expression),
    Window(Expression),
    Threshold(Expression),
    Initial(Expression),
    Unsupported { name: Vec<u8>, value: Expression },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Backend {
    pub name: Vec<u8>,
    pub kind: BackendKind,
    pub properties: Vec<BackendProperty>,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackendKind {
    None,
    Network {
        host: Option<Expression>,
        port: Option<Expression>,
    },
    Unix {
        path: Expression,
    },
    Dynamic,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackendProperty {
    Host(Expression),
    Port(Expression),
    Path(Expression),
    Probe(ProbeReference),
    ConnectTimeout(Expression),
    FirstByteTimeout(Expression),
    BetweenBytesTimeout(Expression),
    MaxConnections(Expression),
    ProxyHeader(Expression),
    Unsupported { name: Vec<u8>, value: Expression },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProbeReference {
    Named { name: Vec<u8>, declaration: usize },
    Inline(Vec<ProbeProperty>),
    Unresolved { name: Vec<u8> },
    Dynamic(Expression),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Director {
    pub name: Vec<u8>,
    pub policy: DirectorKind,
    pub members: Vec<BackendReference>,
    pub unsupported_properties: Vec<Vec<u8>>,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModernDirector {
    pub name: Vec<u8>,
    pub kind: DirectorKind,
    pub constructor: Expression,
    pub methods: Vec<DirectorMethod>,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DirectorKind {
    RoundRobin,
    Random,
    Fallback,
    Hash,
    Unknown(Vec<u8>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DirectorMethod {
    AddBackend {
        backend: BackendReference,
        weight: Option<Expression>,
    },
    BackendLookup {
        arguments: Vec<Expression>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BackendReference {
    Backend {
        name: Vec<u8>,
        declaration: usize,
    },
    Director {
        name: Vec<u8>,
        declaration: usize,
        modern: bool,
        arguments: Vec<Expression>,
    },
    None,
    Unresolved {
        name: Vec<u8>,
    },
    Dynamic(Expression),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Subroutine {
    pub kind: SubroutineKind,
    pub name: Vec<u8>,
    pub statement_ids: Vec<usize>,
    pub provenance: Provenance,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SubroutineKind {
    Init,
    Recv,
    Pipe,
    Pass,
    Hash,
    Purge,
    Hit,
    Miss,
    BackendFetch,
    BackendResponse,
    BackendError,
    Deliver,
    Synth,
    Fini,
    Custom,
}

impl SubroutineKind {
    fn from_name(name: &[u8]) -> Self {
        match name {
            b"vcl_init" => Self::Init,
            b"vcl_recv" => Self::Recv,
            b"vcl_pipe" => Self::Pipe,
            b"vcl_pass" => Self::Pass,
            b"vcl_hash" => Self::Hash,
            b"vcl_purge" => Self::Purge,
            b"vcl_hit" => Self::Hit,
            b"vcl_miss" => Self::Miss,
            b"vcl_backend_fetch" => Self::BackendFetch,
            b"vcl_backend_response" => Self::BackendResponse,
            b"vcl_backend_error" => Self::BackendError,
            b"vcl_deliver" => Self::Deliver,
            b"vcl_synth" => Self::Synth,
            b"vcl_fini" => Self::Fini,
            _ => Self::Custom,
        }
    }

    const fn has_builtin(self) -> bool {
        !matches!(self, Self::Custom)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubroutineComposition {
    pub name: Vec<u8>,
    pub fragments: Vec<usize>,
    pub built_in: BuiltinComposition,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BuiltinComposition {
    AppendedAfterUserFragments,
    None,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CallGraph {
    pub edges: Vec<CallEdge>,
    pub cycles: Vec<Vec<Vec<u8>>>,
    pub truncated: bool,
    pub depth_limited: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CallEdge {
    pub caller: Vec<u8>,
    pub callee: Vec<u8>,
    pub targets: Vec<usize>,
    pub provenance: Provenance,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StatementDecision {
    pub id: usize,
    pub subroutine: usize,
    pub parent: Option<usize>,
    pub depth: usize,
    pub provenance: Provenance,
    pub classification: StatementClassification,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StatementClassification {
    Conditional(Vec<Condition>),
    CacheDecision(FlowAction),
    CacheLifetime(CacheLifetime),
    CacheFlag(CacheFlag),
    BackendSelection(BackendReference),
    HeaderMutation(HeaderMutation),
    Hash(Expression),
    Response(ResponseAction),
    Invalidation(Invalidation),
    Feature(FeatureBehavior),
    NewDirector {
        object: usize,
    },
    DirectorMethod {
        object: usize,
        method: DirectorMethod,
    },
    SubroutineCall {
        name: Vec<u8>,
        targets: Vec<usize>,
    },
    Dynamic(DynamicBehavior),
    Unsupported(UnsupportedBehavior),
    Invalid,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Condition {
    All(Box<Condition>, Box<Condition>),
    Any(Box<Condition>, Box<Condition>),
    Not(Box<Condition>),
    Header {
        scope: HeaderScope,
        name: Vec<u8>,
        operator: ConditionOperator,
        value: Option<Expression>,
    },
    Cookie {
        scope: HeaderScope,
        name: Vec<u8>,
        operator: ConditionOperator,
        value: Option<Expression>,
    },
    Acl {
        value: Expression,
        name: Vec<u8>,
        declaration: Option<usize>,
        negated: bool,
    },
    Comparison {
        left: Expression,
        operator: ConditionOperator,
        right: Expression,
    },
    Value(Expression),
    Dynamic(Expression),
    UnsupportedCall {
        behavior: UnsupportedBehavior,
        expression: Expression,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConditionOperator {
    Exists,
    Equal,
    NotEqual,
    Match,
    NotMatch,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeaderScope {
    Request,
    BackendRequest,
    BackendResponse,
    Response,
    Object,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HeaderMutation {
    pub scope: HeaderScope,
    pub name: Vec<u8>,
    pub operation: HeaderOperation,
    pub value: Option<Expression>,
    pub cookie: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HeaderOperation {
    Set,
    Append,
    Remove,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CacheLifetime {
    pub field: CacheLifetimeField,
    pub operator: AssignmentOperator,
    pub value: Expression,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheLifetimeField {
    Ttl,
    Grace,
    Keep,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CacheFlag {
    HitForPass { duration: Expression },
    Uncacheable(Expression),
    BackgroundFetch(Expression),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlowAction {
    Lookup,
    Hash,
    Pass,
    Pipe,
    Miss,
    Deliver,
    Abandon,
    Restart,
    Retry,
    Fail,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResponseAction {
    Synth {
        status: Option<u16>,
        reason: Option<Expression>,
    },
    Redirect {
        status: u16,
        reason: Option<Expression>,
    },
    SyntheticBody(Expression),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Invalidation {
    Ban(Expression),
    Purge,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FeatureBehavior {
    Esi {
        enabled: Expression,
    },
    Compression {
        operation: CompressionOperation,
        enabled: Expression,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompressionOperation {
    Gzip,
    Gunzip,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DynamicBehavior {
    BackendSelection(Expression),
    Condition(Expression),
    Call {
        function: Vec<u8>,
        arguments: Vec<Expression>,
    },
    Assignment {
        target: Vec<u8>,
        value: Expression,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UnsupportedBehavior {
    VmodCall { module: Vec<u8>, function: Vec<u8> },
    FunctionCall { function: Vec<u8> },
    DirectorMethod { object: Vec<u8>, method: Vec<u8> },
    Assignment { target: Vec<u8> },
    Return { action: Vec<u8> },
    InlineC,
    Declaration,
}

